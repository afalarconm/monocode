//! The Notes surface: a read-only window onto the Obsidian vault. The vault's
//! files are the only copy; nothing here writes to them.
use crate::markdown::{self, MarkdownRow, Part, Tone};
use crate::{
    input::{self, Input},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Role, SharedString, Stateful, Subscription, Task, Window, div, prelude::*, px,
};
use monocode_core::vault::{self, Entry, Frontmatter};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) enum Event {
    Close,
    /// The user chose (or skipped, with an empty `product`) the vault folder
    /// that belongs to `project`.
    MapProduct {
        project: String,
        product: String,
    },
}

type UrlOpener = Box<dyn Fn(&str, &mut App)>;
type PathOpener = Box<dyn Fn(&Path, &mut App)>;

/// A note is cut at this many bytes; a transcript can be megabytes.
const NOTE_CAP_BYTES: usize = 512 * 1024;

struct Loaded {
    front: Option<Frontmatter>,
    /// What the tests assert on: the note's text, and the same text with
    /// wikilinks rewritten to `file://` links as it is rendered.
    #[cfg(test)]
    body: String,
    #[cfg(test)]
    linked: String,
    truncated: bool,
    rows: Vec<MarkdownRow>,
}
enum NoteView {
    Loading,
    /// The file was deleted while it was open.
    Gone,
    Ready(Box<Loaded>),
    Failed(String),
}
struct OpenNote {
    path: PathBuf,
    view: NoteView,
}

pub(crate) struct VaultNotes {
    /// Kept alive so the folder stays watched; dropped when the root changes.
    watch: Option<crate::fs_watch::Watch>,
    listener: Option<Task<()>>,
    open: Option<OpenNote>,
    /// Frontmatter of notes already read, so search can match topic and tags
    /// without reading the whole vault.
    read_fronts: HashMap<PathBuf, Frontmatter>,
    open_url: UrlOpener,
    open_path: PathOpener,
    root: PathBuf,
    project: String,
    mapping: HashMap<String, String>,
    products: Vec<String>,
    scope: Option<String>,
    entries: Vec<Entry>,
    selected: Option<PathBuf>,
    query: Entity<Input>,
    error: Option<String>,
    generation: u64,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Event> for VaultNotes {}

impl Focusable for VaultNotes {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl VaultNotes {
    pub(crate) fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            let mut input = Input::field("vault-search", "Search notes", content(0.35), cx);
            input.set_font_size(13.);
            input
        });
        let typed = cx.subscribe(&query, |_, _, _: &input::Changed, cx| cx.notify());
        let mut this = Self {
            watch: None,
            listener: None,
            open: None,
            read_fronts: HashMap::new(),
            open_url: Box::new(|url, cx| cx.open_url(url)),
            open_path: Box::new(|path, cx| cx.open_with_system(path)),
            root,
            project: String::new(),
            mapping: HashMap::new(),
            products: Vec::new(),
            scope: None,
            entries: Vec::new(),
            selected: None,
            query,
            error: None,
            generation: 0,
            _subscriptions: vec![typed],
        };
        this.refresh(cx);
        this.start_watch(cx);
        this
    }

    /// Called whenever the surface opens or the current repo changes. A new
    /// root restarts the listing; a new repo pre-selects its mapped product.
    pub(crate) fn set_context(
        &mut self,
        root: PathBuf,
        project: String,
        mapping: HashMap<String, String>,
        cx: &mut Context<Self>,
    ) {
        let root_changed = root != self.root;
        let project_changed = project != self.project;
        self.root = root;
        self.project = project;
        self.mapping = mapping;
        if root_changed {
            self.scope = None;
            self.selected = None;
            self.open = None;
        }
        if root_changed {
            self.start_watch(cx);
        }
        if root_changed || project_changed {
            match self.mapping.get(&self.project).filter(|p| !p.is_empty()) {
                Some(product) => self.scope = Some(product.clone()),
                None if project_changed => self.scope = None,
                None => {}
            }
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Lists on the background executor, so the UI thread never walks the vault.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let (root, scope) = (self.root.clone(), self.scope.clone());
        let work = cx.background_executor().spawn(async move {
            let listed = vault::list(&root, scope.as_deref());
            (vault::products(&root), listed)
        });
        cx.spawn(async move |this, cx| {
            let (products, listed) = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.products = products;
                match listed {
                    Ok(entries) => {
                        this.entries = entries;
                        this.error = None;
                    }
                    Err(error) => {
                        this.entries.clear();
                        this.error = Some(error);
                    }
                }
                // A mapped product folder that no longer exists: show everything.
                if let Some(scope) = &this.scope
                    && !this.products.is_empty()
                    && !this.products.contains(scope)
                {
                    this.scope = None;
                    this.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the note off the UI thread, along with a name index for its
    /// wikilinks. Only paths are listed; no other file is read.
    fn select(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selected = Some(path.clone());
        self.open = Some(OpenNote {
            path: path.clone(),
            view: NoteView::Loading,
        });
        self.load(path, cx);
    }

    /// (Re)reads the note, keeping what is on screen until the result arrives.
    fn load(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let root = self.root.clone();
        let work = {
            let (path, root) = (path.clone(), root.clone());
            cx.background_executor().spawn(async move {
                let note = vault::read_note(&path, NOTE_CAP_BYTES);
                let index = vault::NameIndex::new(&vault::list(&root, None).unwrap_or_default());
                (note, index)
            })
        };
        cx.spawn(async move |this, cx| {
            let (note, index) = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.open.as_ref().map(|open| &open.path) != Some(&path) {
                    return;
                }
                let view = match note {
                    Ok(note) => {
                        if let Some(front) = &note.front {
                            this.read_fronts.insert(path.clone(), front.clone());
                        }
                        NoteView::Ready(Box::new(load(&root, &path, note, &index)))
                    }
                    Err(error) => NoteView::Failed(error),
                };
                this.open = Some(OpenNote { path, view });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A raw transcript's summary lives one folder up under the same name.
    fn summary_of(path: &Path) -> Option<PathBuf> {
        let raw = path
            .parent()
            .filter(|d| d.file_name().is_some_and(|n| n == "raw"))?;
        let sessions = raw
            .parent()
            .filter(|d| d.file_name().is_some_and(|n| n == "sessions"))?;
        Some(sessions.join(path.file_name()?)).filter(|p| p.is_file())
    }

    fn start_watch(&mut self, cx: &mut Context<Self>) {
        self.watch = None;
        self.listener = None;
        // Real file events would wake the test scheduler from outside it, so
        // tests drive `listen` with their own channel instead.
        if cfg!(test) {
            return;
        }
        if let Some((watch, changes)) = crate::fs_watch::watch(std::slice::from_ref(&self.root)) {
            self.watch = Some(watch);
            self.listen(changes, cx);
        }
    }

    /// Re-reads on each change signal once a burst of writes has settled.
    fn listen(&mut self, changes: async_channel::Receiver<()>, cx: &mut Context<Self>) {
        self.listener = Some(cx.spawn(async move |this, cx| {
            while changes.recv().await.is_ok() {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                while changes.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.on_disk_change(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Lists again and re-reads the open note; a deleted one says so.
    fn on_disk_change(&mut self, cx: &mut Context<Self>) {
        self.refresh(cx);
        let Some(path) = self.open.as_ref().map(|open| open.path.clone()) else {
            return;
        };
        if path.is_file() {
            self.load(path, cx);
        } else {
            self.open = Some(OpenNote {
                path,
                view: NoteView::Gone,
            });
            cx.notify();
        }
    }

    fn set_scope(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.selected = None;
            self.refresh(cx);
        }
        cx.notify();
    }

    fn needs_mapping(&self) -> bool {
        !self.project.is_empty()
            && !self.products.is_empty()
            && !self.mapping.contains_key(&self.project)
    }

    /// Remembers the answer (an empty product means "skip") and scopes to it.
    fn map_product(&mut self, product: String, cx: &mut Context<Self>) {
        self.mapping.insert(self.project.clone(), product.clone());
        cx.emit(Event::MapProduct {
            project: self.project.clone(),
            product: product.clone(),
        });
        if !product.is_empty() {
            self.set_scope(Some(product), cx);
        }
        cx.notify();
    }

    /// The rows the list shows: entries whose name matches the search box.
    fn visible(&self, cx: &App) -> Vec<&Entry> {
        let query = self.query.read(cx).content.to_lowercase();
        let query = query.trim();
        self.entries
            .iter()
            .filter(|e| {
                query.is_empty()
                    || e.name.to_lowercase().contains(query)
                    || self.read_fronts.get(&e.path).is_some_and(|front| {
                        front
                            .topic
                            .as_deref()
                            .is_some_and(|t| t.to_lowercase().contains(query))
                            || front.tags.iter().any(|t| t.to_lowercase().contains(query))
                    })
            })
            .collect()
    }

    /// The folder an entry sits in, relative to what the tree is rooted at.
    fn dir_label(&self, entry: &Entry) -> String {
        let parent = entry.rel.parent().unwrap_or(Path::new(""));
        let base = self
            .scope
            .as_ref()
            .map(|product| Path::new("20-Projects").join(product));
        let shown = base
            .as_ref()
            .and_then(|base| parent.strip_prefix(base).ok())
            .unwrap_or(parent);
        shown.display().to_string()
    }

    fn chips(&self, cx: &mut Context<Self>) -> Div {
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .px_3()
            .py_2()
            .child(
                chip("All".into(), self.scope.is_none(), "vault-chip-all".into())
                    .on_click(cx.listener(|this, _, _, cx| this.set_scope(None, cx))),
            );
        for product in &self.products {
            let target = product.clone();
            row = row.child(
                chip(
                    product.clone(),
                    self.scope.as_ref() == Some(product),
                    format!("vault-chip-{product}"),
                )
                .on_click(
                    cx.listener(move |this, _, _, cx| this.set_scope(Some(target.clone()), cx)),
                ),
            );
        }
        row
    }

    fn prompt(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut prompt = div()
            .id("vault-map-prompt")
            .debug_selector(|| "vault-map-prompt".into())
            .mx_3()
            .mb_2()
            .p_2()
            .rounded_lg()
            .bg(content(0.05))
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1p5()
            .text_size(px(12.))
            .text_color(content(0.7))
            .child(format!(
                "Which product is {}?",
                crate::workspace::project_name(&self.project)
            ));
        for product in &self.products {
            let target = product.clone();
            prompt = prompt.child(
                chip(product.clone(), false, format!("vault-map-{product}")).on_click(
                    cx.listener(move |this, _, _, cx| this.map_product(target.clone(), cx)),
                ),
            );
        }
        prompt.child(
            chip("Skip".into(), false, "vault-map-skip".into())
                .on_click(cx.listener(|this, _, _, cx| this.map_product(String::new(), cx))),
        )
    }

    fn detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(open) = &self.open else {
            return div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .text_size(px(13.))
                .text_color(content(0.45))
                .child(icon("note", 24.))
                .child("Select a note")
                .into_any_element();
        };
        let title = match &open.view {
            NoteView::Ready(note) => note.front.as_ref().and_then(|f| f.topic.clone()),
            _ => None,
        }
        .or_else(|| {
            open.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
        let back = Self::summary_of(&open.path);
        let mut actions = div().flex().flex_wrap().items_center().gap_1p5();
        if let Some(summary) = back {
            actions = actions.child(
                chip(
                    "Back to summary".into(),
                    false,
                    "vault-back-to-summary".into(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select(summary.clone(), cx))),
            );
        }
        actions = actions
            .child(
                chip(
                    "Open in Obsidian".into(),
                    false,
                    "vault-open-obsidian".into(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(open) = &this.open {
                        let url = vault::obsidian_url(&this.root, &open.path);
                        (this.open_url)(&url, cx);
                    }
                })),
            )
            .child(
                chip(
                    "Open with default app".into(),
                    false,
                    "vault-open-default".into(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(open) = &this.open {
                        (this.open_path)(&open.path, cx);
                    }
                })),
            );
        let body: AnyElement = match &open.view {
            NoteView::Gone => div()
                .id("vault-gone")
                .debug_selector(|| "vault-gone".into())
                .p_4()
                .text_size(px(13.))
                .text_color(content(0.55))
                .child("This note no longer exists.")
                .into_any_element(),
            NoteView::Loading => div()
                .p_4()
                .text_size(px(13.))
                .text_color(content(0.45))
                .child("Loading…")
                .into_any_element(),
            NoteView::Failed(error) => div()
                .id("vault-error")
                .debug_selector(|| "vault-error".into())
                .p_4()
                .text_size(px(13.))
                .text_color(content(0.55))
                .child(error.clone())
                .into_any_element(),
            NoteView::Ready(note) => {
                let lines = note.front.as_ref().map(meta_lines).unwrap_or_default();
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when(!lines.is_empty(), |d| {
                        d.child(
                            div()
                                .id("vault-meta")
                                .debug_selector(|| "vault-meta".into())
                                .px_4()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .text_size(px(12.))
                                .text_color(content(0.5))
                                .children(lines),
                        )
                    })
                    .when(note.truncated, |d| {
                        d.child(
                            div()
                                .id("vault-truncated")
                                .debug_selector(|| "vault-truncated".into())
                                .px_4()
                                .py_1()
                                .text_size(px(12.))
                                .text_color(content(0.55))
                                .child(format!(
                                    "Showing the first {} KB of this note.",
                                    NOTE_CAP_BYTES / 1024
                                )),
                        )
                    })
                    .child(
                        div()
                            .id("vault-body")
                            .debug_selector(|| "vault-body".into())
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_4()
                            .pb_4()
                            .children(note_rows(&note.rows)),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(actions),
            )
            .child(body)
            .into_any_element()
    }

    fn list(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let visible: Vec<Entry> = self.visible(cx).into_iter().cloned().collect();
        // Folders in order of their newest note; rows stay newest first inside.
        let mut groups: Vec<(String, Vec<Entry>)> = Vec::new();
        for entry in visible {
            let label = self.dir_label(&entry);
            match groups.iter_mut().find(|(name, _)| *name == label) {
                Some((_, rows)) => rows.push(entry),
                None => groups.push((label, vec![entry])),
            }
        }
        let mut children: Vec<AnyElement> = Vec::new();
        let mut index: usize = 0;
        for (label, rows) in groups {
            if !label.is_empty() {
                children.push(
                    div()
                        .px(px(10.))
                        .pt_2()
                        .pb_1()
                        .truncate()
                        .text_size(px(11.))
                        .text_color(content(0.4))
                        .child(label)
                        .into_any_element(),
                );
            }
            for entry in rows {
                let ix = index;
                index += 1;
                let active = self.selected.as_ref() == Some(&entry.path);
                let path = entry.path.clone();
                children.push(
                    div()
                        .id(("vault-row", ix))
                        .debug_selector(move || format!("vault-row-{ix}"))
                        .role(Role::Button)
                        .aria_label(entry.name.clone())
                        .aria_selected(active)
                        .px(px(10.))
                        .py_1p5()
                        .rounded_md()
                        .text_size(px(13.))
                        .text_color(content(0.85))
                        .hover(|s| s.bg(content(0.05)))
                        .when(active, |d| d.bg(content(0.1)))
                        .child(div().truncate().child(entry.name))
                        .on_click(cx.listener(move |this, _, _, cx| this.select(path.clone(), cx)))
                        .into_any_element(),
                );
            }
        }
        div()
            .id("vault-list")
            .role(Role::ListBox)
            .aria_label("Vault notes")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(6.))
            .pb_2()
            .when(children.is_empty(), |d| {
                d.child(
                    div()
                        .px_1p5()
                        .py_2()
                        .text_size(px(12.))
                        .text_color(content(0.5))
                        .child("No notes found"),
                )
            })
            .children(children)
    }
}

impl gpui::Render for VaultNotes {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.error.is_some() {
            div()
                .id("vault-missing")
                .debug_selector(|| "vault-missing".into())
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_6()
                .text_size(px(13.))
                .text_color(content(0.55))
                .child(format!("Can't read the vault at {}.", self.root.display()))
                .child("Set the folder in Settings, General, Vault folder.")
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .w(px(320.))
                        .h_full()
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(content(0.07))
                        .child(self.chips(cx))
                        .when(self.needs_mapping(), |d| d.child(self.prompt(cx)))
                        .child(
                            div()
                                .h(px(36.))
                                .flex_shrink_0()
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_b_1()
                                .border_color(content(0.07))
                                .child(icon("search", 12.))
                                .child(div().flex_1().min_w_0().child(self.query.clone())),
                        )
                        .child(self.list(cx)),
                )
                .child(self.detail(cx))
                .into_any_element()
        };
        div()
            .id("vault-notes")
            .role(Role::Region)
            .aria_label("Notes")
            .size_full()
            .flex()
            .flex_col()
            .text_color(content(1.))
            // Links to vault notes open here; anything else goes on to the app.
            .on_action(
                cx.listener(|this, event: &crate::file_editor::OpenPath, _, cx| {
                    let (path, _) =
                        crate::file_editor::file_location(&event.0, &this.root.to_string_lossy());
                    let path = PathBuf::from(path);
                    if path.starts_with(&this.root)
                        && path.extension().is_some_and(|e| e == "md")
                        && path.is_file()
                    {
                        this.select(path, cx);
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .capture_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(Event::Close);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }
}

/// The metadata header's lines: topic, when, status, tool and tags.
fn meta_lines(front: &Frontmatter) -> Vec<String> {
    let mut lines = Vec::new();
    lines.extend(front.topic.clone());
    let when: Vec<&str> = [front.date.as_deref(), front.time.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !when.is_empty() {
        lines.push(when.join(" "));
    }
    if let Some(status) = &front.status {
        lines.push(format!("status: {status}"));
    }
    let made: Vec<&str> = [front.tool.as_deref(), front.model.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    if !made.is_empty() {
        lines.push(made.join(" · "));
    }
    if !front.tags.is_empty() {
        lines.push(
            front
                .tags
                .iter()
                .map(|tag| format!("#{tag}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    lines
}

/// Rewrites `[[target|alias]]` to a link the markdown renderer opens, or to
/// plain text when the target does not resolve.
fn link_wikilinks(text: &str, resolve: &dyn Fn(&str) -> Option<PathBuf>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("]]").filter(|end| !after[..*end].contains('\n')) else {
            out.push_str("[[");
            rest = after;
            continue;
        };
        let (target, alias) = match after[..end].split_once('|') {
            Some((target, alias)) => (target, Some(alias.trim())),
            None => (&after[..end], None),
        };
        let name = target.split('#').next().unwrap_or(target).trim();
        let shown = alias.filter(|a| !a.is_empty()).unwrap_or(name);
        match resolve(name) {
            Some(path) => out.push_str(&format!("[{shown}]({})", vault::file_url(&path))),
            None => out.push_str(shown),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Turns a read note into what the pane renders: wikilinks rewritten, then
/// the shared markdown rows with links resolved against the note's folder.
fn load(root: &Path, path: &Path, note: vault::Note, index: &vault::NameIndex) -> Loaded {
    let linked = link_wikilinks(&note.body, &|name| {
        vault::resolve_wikilink(root, path, name, index)
    });
    let base = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut rows = Vec::new();
    for block in &crate::markdown_model::parse_full_hard_breaks(&linked).blocks {
        for mut row in markdown::rows(&block.block) {
            markdown::resolve_file_links(&mut row, &base);
            rows.push(row);
        }
    }
    Loaded {
        front: note.front,
        #[cfg(test)]
        body: note.body,
        #[cfg(test)]
        linked,
        truncated: note.truncated,
        rows,
    }
}

/// The same row rendering the database notes use for their preview.
fn note_rows(rows: &[MarkdownRow]) -> Vec<AnyElement> {
    rows.iter()
        .enumerate()
        .map(|(ix, row)| {
            let id: SharedString = format!("vault-markdown-{ix}").into();
            let body: AnyElement = match &row.part {
                Part::Code { fence, lines } => div()
                    .id(id.clone())
                    .overflow_x_scroll()
                    .rounded_lg()
                    .border_1()
                    .border_color(content(0.1))
                    .bg(content(0.03))
                    .py_2()
                    .child(markdown::code_lines(fence, lines.clone(), None, "", false))
                    .into_any_element(),
                _ => markdown::body(row, id, Tone::Normal, None, Instant::now()),
            };
            div()
                .pt(px(row.gap))
                .flex()
                .min_w_0()
                .gap_2()
                .pl(px(row.indent as f32 * 16.))
                .when(row.quote > 0, |d| {
                    d.border_l_2().border_color(content(0.15)).pl_3()
                })
                .when_some(row.marker.clone(), |d, marker| {
                    d.child(
                        div()
                            .w_4()
                            .flex_shrink_0()
                            .text_color(content(0.7))
                            .text_size(px(14.))
                            .line_height(px(24.))
                            .child(marker),
                    )
                })
                .child(div().flex_1().min_w_0().child(body))
                .into_any_element()
        })
        .collect()
}

/// A filter or choice pill. The selector doubles as its element id.
fn chip(label: String, active: bool, selector: String) -> Stateful<Div> {
    let id = SharedString::from(selector.clone());
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .role(Role::Button)
        .aria_label(label.clone())
        .aria_selected(active)
        .cursor_pointer()
        .px_2p5()
        .py_1()
        .rounded_full()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(content(if active { 1. } else { 0.55 }))
        .bg(content(if active { 0.12 } else { 0.0 }))
        .hover(|s| s.bg(content(0.08)))
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, Pixels, TestAppContext, VisualTestContext, px, size};
    use std::{cell::RefCell, path::Path, rc::Rc, time::Duration};

    type MapLog = Rc<RefCell<Vec<(String, String)>>>;

    /// A temp folder that removes itself; the GUI crate has no `tempfile`.
    pub(super) struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("monocode-vault-notes-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
    fn write(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# note\n").unwrap();
        // Undated files sort by modified time; pin it far in the past.
        if !rel.contains("sessions/") {
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_577_836_800))
                .unwrap();
        }
    }
    /// A small vault: two products, three dated summaries, two undated files.
    fn vault() -> (TempDir, PathBuf) {
        let tmp = tempdir();
        let root = tmp.0.join("Obsidian");
        write(
            &root,
            "20-Projects/geodb/sessions/2026-10-13-15_26-geodb-mcp-spike.md",
        );
        write(
            &root,
            "20-Projects/geodb/sessions/2026-10-19-09_00-sigex-batch.md",
        );
        write(&root, "20-Projects/geodb/README.md");
        write(
            &root,
            "20-Projects/opportunity/sessions/2026-10-15-10_00-alerts.md",
        );
        write(&root, "home.md");
        (tmp, root)
    }
    fn open<'a>(
        cx: &'a mut TestAppContext,
        root: &Path,
    ) -> (Entity<VaultNotes>, &'a mut VisualTestContext) {
        let root = root.to_path_buf();
        let (view, cx) = cx.add_window_view(|_, cx| VaultNotes::new(root, cx));
        cx.simulate_resize(size(px(1000.), px(700.)));
        cx.run_until_parked();
        (view, cx)
    }
    fn names(view: &Entity<VaultNotes>, cx: &mut VisualTestContext) -> Vec<String> {
        view.read_with(cx, |v, _| {
            v.entries.iter().map(|e| e.name.clone()).collect()
        })
    }
    fn bounds(cx: &mut VisualTestContext, selector: &'static str) -> Option<Bounds<Pixels>> {
        cx.debug_bounds(selector)
    }
    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let at = bounds(cx, selector)
            .unwrap_or_else(|| panic!("{selector} is not on screen"))
            .center();
        cx.simulate_click(at, Default::default());
        cx.run_until_parked();
    }
    fn mapped(events: &MapLog) -> Vec<(String, String)> {
        events.borrow().clone()
    }
    fn watch(
        view: &Entity<VaultNotes>,
        cx: &mut VisualTestContext,
    ) -> (MapLog, gpui::Subscription) {
        let log = Rc::new(RefCell::new(Vec::new()));
        let sink = log.clone();
        let sub = cx.update(|_, cx| {
            cx.subscribe(view, move |_, event: &Event, _| {
                if let Event::MapProduct { project, product } = event {
                    sink.borrow_mut().push((project.clone(), product.clone()));
                }
            })
        });
        (log, sub)
    }

    #[gpui::test]
    fn lists_the_vault_newest_first_with_all_selected(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        assert_eq!(
            names(&view, cx),
            [
                "2026-10-19-09_00-sigex-batch",
                "2026-10-15-10_00-alerts",
                "2026-10-13-15_26-geodb-mcp-spike",
                "README",
                "home"
            ]
        );
        assert!(view.read_with(cx, |v, _| v.scope.is_none()));
        assert!(bounds(cx, "vault-chip-all").is_some());
        assert!(bounds(cx, "vault-row-0").is_some());
    }

    #[gpui::test]
    fn a_product_chip_scopes_the_list_and_roots_the_tree(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        click(cx, "vault-chip-geodb");
        assert_eq!(
            names(&view, cx),
            [
                "2026-10-19-09_00-sigex-batch",
                "2026-10-13-15_26-geodb-mcp-spike",
                "README"
            ]
        );
        assert_eq!(
            view.read_with(cx, |v, _| v.scope.clone()),
            Some("geodb".to_owned())
        );
    }

    #[gpui::test]
    fn the_mapped_product_is_preselected(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        let mapping = HashMap::from([("/r/a".to_owned(), "geodb".to_owned())]);
        view.update(cx, |v, cx| {
            v.set_context(root.clone(), "/r/a".into(), mapping, cx)
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.scope.clone()),
            Some("geodb".to_owned())
        );
        assert_eq!(names(&view, cx).len(), 3);
    }

    #[gpui::test]
    fn an_unmapped_repo_shows_the_inline_prompt_and_skip_persists_empty(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        let (log, _sub) = watch(&view, cx);
        view.update(cx, |v, cx| {
            v.set_context(root.clone(), "/r/a".into(), HashMap::new(), cx)
        });
        cx.run_until_parked();
        assert!(bounds(cx, "vault-map-prompt").is_some());
        click(cx, "vault-map-skip");
        assert_eq!(mapped(&log), [("/r/a".to_owned(), String::new())]);
        assert!(bounds(cx, "vault-map-prompt").is_none());
        // A second repo is asked separately; choosing a product maps and scopes it.
        view.update(cx, |v, cx| {
            v.set_context(root.clone(), "/r/b".into(), v.mapping.clone(), cx)
        });
        cx.run_until_parked();
        click(cx, "vault-map-geodb");
        assert_eq!(
            mapped(&log).last().unwrap(),
            &("/r/b".to_owned(), "geodb".to_owned())
        );
        assert_eq!(
            view.read_with(cx, |v, _| v.scope.clone()),
            Some("geodb".to_owned())
        );
        assert!(bounds(cx, "vault-map-prompt").is_none());
    }

    #[gpui::test]
    fn search_matches_file_names_case_insensitively(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        let query = view.read_with(cx, |v, _| v.query.clone());
        query.update(cx, |input, cx| input.set_text("SIGEX".into(), cx));
        cx.run_until_parked();
        let found: Vec<String> = view.read_with(cx, |v, cx| {
            v.visible(cx).iter().map(|e| e.name.clone()).collect()
        });
        assert_eq!(found, ["2026-10-19-09_00-sigex-batch"]);
    }

    #[gpui::test]
    fn a_missing_vault_says_so_and_does_not_panic(cx: &mut TestAppContext) {
        let (view, cx) = open(cx, Path::new("/nope/not/here"));
        assert!(bounds(cx, "vault-missing").is_some());
        assert!(names(&view, cx).is_empty());
    }

    fn write_text(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn summary(root: &Path) -> PathBuf {
        root.join("20-Projects/geodb/sessions/2026-10-19-09_00-sigex-batch.md")
    }
    fn raw(root: &Path) -> PathBuf {
        root.join("20-Projects/geodb/sessions/raw/2026-10-19-09_00-sigex-batch.md")
    }
    fn select(view: &Entity<VaultNotes>, cx: &mut VisualTestContext, path: &Path) {
        let path = path.to_path_buf();
        view.update(cx, |v, cx| v.select(path, cx));
        cx.run_until_parked();
    }
    /// What the open note loaded: (frontmatter, body, linked body, truncated).
    fn loaded(
        view: &Entity<VaultNotes>,
        cx: &mut VisualTestContext,
    ) -> (Option<Frontmatter>, String, String, bool) {
        view.read_with(cx, |v, _| {
            match &v.open.as_ref().expect("a note is open").view {
                NoteView::Ready(note) => (
                    note.front.clone(),
                    note.body.clone(),
                    note.linked.clone(),
                    note.truncated,
                ),
                NoteView::Loading | NoteView::Gone => panic!("not loaded"),
                NoteView::Failed(error) => panic!("failed to load: {error}"),
            }
        })
    }

    #[test]
    fn unresolved_wikilinks_become_plain_text_and_aliases_are_used() {
        let resolve = |t: &str| (t == "x").then(|| PathBuf::from("/v/x y.md"));
        assert_eq!(
            link_wikilinks("a [[x|Shown]] b [[nope]] c [[x#Head]]", &resolve),
            "a [Shown](file:///v/x%20y.md) b nope c [x](file:///v/x%20y.md)"
        );
    }

    #[gpui::test]
    fn selecting_a_summary_shows_its_metadata_header_and_body(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        write_text(
            &summary(&root),
            "---\ndate: 2026-10-19\ntime: \"09:00\"\ntopic: sigex-batch\nstatus: landed\ntags: [geodb, sigex]\n---\n# Heading\n\nBody text\n",
        );
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        let (front, body, _, truncated) = loaded(&view, cx);
        let lines = meta_lines(&front.expect("frontmatter parsed"));
        for wanted in ["sigex-batch", "2026-10-19", "landed", "#geodb"] {
            assert!(
                lines.iter().any(|l| l.contains(wanted)),
                "{wanted} missing from {lines:?}"
            );
        }
        assert!(!body.contains("---"));
        assert!(!truncated);
        assert!(bounds(cx, "vault-meta").is_some());
        assert!(bounds(cx, "vault-body").is_some());
    }

    #[gpui::test]
    fn a_note_without_frontmatter_has_no_header(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        write_text(&summary(&root), "# Plain\n\nJust text\n");
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        assert!(loaded(&view, cx).0.is_none());
        assert!(bounds(cx, "vault-meta").is_none());
        assert!(bounds(cx, "vault-body").is_some());
    }

    #[gpui::test]
    fn clicking_a_wikilink_opens_the_target(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        write_text(
            &summary(&root),
            "See [[raw/2026-10-19-09_00-sigex-batch]] and [[nowhere]].\n",
        );
        write(
            &root,
            "20-Projects/geodb/sessions/raw/2026-10-19-09_00-sigex-batch.md",
        );
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        let (_, _, linked, _) = loaded(&view, cx);
        assert!(linked.contains(&monocode_core::vault::file_url(&raw(&root))));
        assert!(!linked.contains("[["));
        assert!(linked.contains("nowhere"));
        // The renderer opens a link by dispatching OpenPath from inside the surface.
        view.update_in(cx, |v, window, cx| {
            window.focus(&v.query.focus_handle(cx), cx)
        });
        cx.dispatch_action(crate::file_editor::OpenPath(
            monocode_core::vault::file_url(&raw(&root)),
        ));
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.selected.clone()),
            Some(raw(&root))
        );
    }

    #[gpui::test]
    fn a_raw_note_offers_a_link_back_to_its_summary(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        write(
            &root,
            "20-Projects/geodb/sessions/raw/2026-10-19-09_00-sigex-batch.md",
        );
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        assert!(bounds(cx, "vault-back-to-summary").is_none());
        select(&view, cx, &raw(&root));
        click(cx, "vault-back-to-summary");
        assert_eq!(
            view.read_with(cx, |v, _| v.selected.clone()),
            Some(summary(&root))
        );
    }

    #[gpui::test]
    fn a_huge_note_is_cut_and_says_so(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        write_text(&summary(&root), &"x".repeat(2 * 1024 * 1024));
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        let (_, body, _, truncated) = loaded(&view, cx);
        assert!(truncated);
        assert!(body.len() <= NOTE_CAP_BYTES);
        assert!(bounds(cx, "vault-truncated").is_some());
    }

    #[gpui::test]
    fn an_unreadable_note_shows_an_error_row(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        std::fs::write(summary(&root), [0xff, 0xfe]).unwrap();
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        assert!(bounds(cx, "vault-error").is_some());
        assert!(bounds(cx, "vault-body").is_none());
    }

    #[gpui::test]
    fn open_in_obsidian_and_default_app_use_the_injected_openers(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        let urls = Rc::new(RefCell::new(Vec::<String>::new()));
        let paths = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
        let (u, p) = (urls.clone(), paths.clone());
        view.update(cx, |v, _| {
            v.open_url = Box::new(move |url, _| u.borrow_mut().push(url.to_owned()));
            v.open_path = Box::new(move |path, _| p.borrow_mut().push(path.to_path_buf()));
        });
        select(&view, cx, &summary(&root));
        click(cx, "vault-open-obsidian");
        click(cx, "vault-open-default");
        assert_eq!(
            *urls.borrow(),
            [monocode_core::vault::obsidian_url(&root, &summary(&root))]
        );
        assert_eq!(*paths.borrow(), [summary(&root)]);
    }

    #[gpui::test]
    fn search_also_matches_topic_and_tags_of_notes_already_read(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let front = "---\ntopic: zebra-topic\ntags: [quokka]\n---\nbody\n";
        write_text(&summary(&root), front);
        write_text(
            &root.join("20-Projects/opportunity/sessions/2026-10-15-10_00-alerts.md"),
            front,
        );
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        let query = view.read_with(cx, |v, _| v.query.clone());
        for term in ["zebra", "QUOKKA"] {
            query.update(cx, |input, cx| input.set_text(term.into(), cx));
            cx.run_until_parked();
            let found: Vec<String> = view.read_with(cx, |v, cx| {
                v.visible(cx).iter().map(|e| e.name.clone()).collect()
            });
            // The never-opened note has the same frontmatter but was not read.
            assert_eq!(found, ["2026-10-19-09_00-sigex-batch"], "searching {term}");
        }
    }

    #[gpui::test]
    fn a_new_file_appears_without_a_manual_refresh(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        let before = names(&view, cx).len();
        let (tx, rx) = async_channel::bounded(1);
        view.update(cx, |v, cx| v.listen(rx, cx));
        write(
            &root,
            "20-Projects/geodb/sessions/2026-10-25-08_00-fresh.md",
        );
        tx.try_send(()).unwrap();
        // Nothing happens before the debounce has passed.
        cx.run_until_parked();
        assert_eq!(names(&view, cx).len(), before);
        cx.executor().advance_clock(Duration::from_millis(400));
        cx.run_until_parked();
        assert!(names(&view, cx).contains(&"2026-10-25-08_00-fresh".to_owned()));
    }

    #[gpui::test]
    fn a_deleted_selected_note_says_it_no_longer_exists(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let (view, cx) = open(cx, &root);
        select(&view, cx, &summary(&root));
        assert!(bounds(cx, "vault-body").is_some());
        std::fs::remove_file(summary(&root)).unwrap();
        view.update(cx, |v, cx| v.on_disk_change(cx));
        cx.run_until_parked();
        assert!(bounds(cx, "vault-gone").is_some());
        assert!(bounds(cx, "vault-body").is_none());
        assert!(!names(&view, cx).contains(&"2026-10-19-09_00-sigex-batch".to_owned()));
    }

    #[gpui::test]
    fn a_half_written_file_does_not_break_refresh(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let path = root.join("20-Projects/geodb/sessions/2026-10-26-08_00-wip.md");
        write_text(&path, "");
        let (view, cx) = open(cx, &root);
        select(&view, cx, &path);
        assert_eq!(loaded(&view, cx).1, "");
        write_text(&path, "# Done writing\n");
        view.update(cx, |v, cx| v.on_disk_change(cx));
        cx.run_until_parked();
        assert_eq!(loaded(&view, cx).1, "# Done writing\n");
        assert!(bounds(cx, "vault-body").is_some());
    }

    #[gpui::test]
    fn changing_the_root_restarts_the_watch_and_the_list(cx: &mut TestAppContext) {
        let (_tmp, root) = vault();
        let other_tmp = tempdir();
        let other = other_tmp.0.join("Other");
        write(&other, "20-Projects/solo/sessions/2026-10-01-08_00-only.md");
        let (view, cx) = open(cx, &root);
        view.update(cx, |v, cx| {
            v.set_context(other.clone(), String::new(), HashMap::new(), cx)
        });
        cx.run_until_parked();
        assert_eq!(names(&view, cx), ["2026-10-01-08_00-only"]);
        assert!(view.read_with(cx, |v, _| v.open.is_none()));
    }
}
