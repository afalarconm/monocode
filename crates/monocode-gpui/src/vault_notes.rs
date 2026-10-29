//! The Notes surface: a read-only window onto the Obsidian vault. The vault's
//! files are the only copy; nothing here writes to them.
use crate::{
    input::{self, Input},
    theme::*,
};
use gpui::{
    AnyElement, App, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    IntoElement, Role, SharedString, Stateful, Subscription, Window, div, prelude::*, px,
};
use monocode_core::vault::{self, Entry};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
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

pub(crate) struct VaultNotes {
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
            .filter(|e| query.is_empty() || e.name.to_lowercase().contains(query))
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = Some(path.clone());
                            cx.notify();
                        }))
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
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .text_size(px(13.))
                        .text_color(content(0.45))
                        .child(icon("note", 24.))
                        .child("Select a note"),
                )
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
            .capture_key_down(cx.listener(|_, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.emit(Event::Close);
                    cx.stop_propagation();
                }
            }))
            .child(body)
    }
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
    fn mapped(events: &Rc<RefCell<Vec<(String, String)>>>) -> Vec<(String, String)> {
        events.borrow().clone()
    }
    fn watch(
        view: &Entity<VaultNotes>,
        cx: &mut VisualTestContext,
    ) -> (Rc<RefCell<Vec<(String, String)>>>, gpui::Subscription) {
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
}
