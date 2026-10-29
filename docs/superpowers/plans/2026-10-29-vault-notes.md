# Vault Notes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the database-backed Notes surface with a read-only browser over Federico's Obsidian vault.

**Architecture:** Pure vault logic (listing, sorting, frontmatter, wikilinks, reading) lives in `monocode-core/src/vault.rs` and is unit-tested without a GUI. A new GPUI entity `VaultNotes` in `monocode-gpui/src/vault_notes.rs` renders it and replaces `Entity<Notes>` behind `Shell::notes`. The old Notes code stays compiled but unreachable.

**Tech Stack:** Rust, GPUI, `notify` (already used by `fs_watch.rs`), `tempfile` in tests. No new dependencies; frontmatter is hand-parsed.

**Spec:** `docs/superpowers/specs/2026-10-29-vault-notes-design.md`

## Global Constraints

- Read-only: nothing in this feature ever writes to the vault.
- Never read the contents of every vault file. List paths only; read a file's contents only when it is on screen, open, or already read for search.
- `sessions/raw/` is hidden from the tree and from search; reachable only through a link.
- Default vault root: `/Users/federicoalarcon/ObsidianVaults/Obsidian` (configurable in Settings).
- Session file names: `YYYY-MM-DD-HH_MM-topic.md`, legacy `DD-MM-YYYY-HH_MM-topic.md`. Never rename.
- Do not read `~/.claude/skills/*/vault-config.md`. The repo-to-product mapping is Monocode's own state.
- Rebase-friendly: new modules; touch existing Notes wiring as little as possible.
- Lint: `cargo clippy --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-core -p monocode-gpui --all-targets --profile native-dev -- -D warnings`. The full GUI suite must stay green.
- Commit messages carry no attribution lines.

## Review Focus

1. Vault folder missing or unreadable (moved, unplugged): a plain message pointing to the Settings field, no panic. Pinned in Task 3.
2. A note with no frontmatter, or an unterminated `---` block: body renders, no metadata header. Pinned in Task 1.
3. A multi-megabyte raw transcript opened through a link: rendered up to a cap and says it was cut. Pinned in Task 4.
4. A file deleted or half-written by an agent while open or listed: refresh never crashes; a deleted selected note says it no longer exists. Pinned in Task 5.
5. Names with spaces, unicode, `%`, `#` in file names, wikilinks and the `obsidian://` URL. Pinned in Task 1.

---

## File Structure

- Create `crates/monocode-core/src/vault.rs`: pure logic (Task 1). Register in `crates/monocode-core/src/lib.rs` as `pub mod vault;`.
- Create `crates/monocode-gpui/src/vault_notes.rs`: the entity (Tasks 3-5). Register as `mod vault_notes;` in `main.rs` beside `mod notes;`.
- Modify `crates/monocode-gpui/src/settings/preferences.rs`, `settings/management.rs` or the page that renders path rows (Task 2).
- Modify `crates/monocode-gpui/src/workspace.rs`: `vault_products` map (Task 2).
- Modify `crates/monocode-gpui/src/fs_watch.rs`: ignore `.obsidian` and `.trash` (Task 5).
- Modify `crates/monocode-gpui/src/shell.rs` (field type) and `shell/app_views.rs` (`open_notes`, `leave_app_views`) (Task 6).

---

### Task 1: Vault logic in `monocode-core`

**Files:**
- Create: `crates/monocode-core/src/vault.rs`
- Modify: `crates/monocode-core/src/lib.rs` (add `pub mod vault;`)
- Test: inline `#[cfg(test)] mod tests` in `vault.rs`, using `tempfile::tempdir()`

**Interfaces:**
- Produces:
  - `pub struct Entry { pub path: PathBuf, pub rel: PathBuf, pub name: String, pub sort_key: u64 }`; `sort_key` is `YYYYMMDDHHMM` from the file name when present, else the file's modified time converted to the same scale. Higher is newer.
  - `pub fn filename_key(name: &str) -> Option<u64>`: parses `YYYY-MM-DD-HH_MM-...` and `DD-MM-YYYY-HH_MM-...`.
  - `pub fn products(root: &Path) -> Vec<String>`: directory names directly under `20-Projects/`, sorted.
  - `pub fn list(root: &Path, product: Option<&str>) -> Result<Vec<Entry>, String>`: every `.md` file under `root` (or under `20-Projects/<product>/`), newest first. Skips any directory whose name starts with `.`, skips a `raw` directory that sits inside a `sessions` directory, never follows symlinks. `Err` only when `root` cannot be read.
  - `#[derive(Default)] pub struct Frontmatter { pub date, pub time, pub topic, pub project, pub status, pub tool, pub model, pub branch: Option<String>, pub tags: Vec<String> }`
  - `pub struct Note { pub front: Option<Frontmatter>, pub body: String, pub truncated: bool }`
  - `pub fn read_note(path: &Path, cap_bytes: usize) -> Result<Note, String>`: reads at most `cap_bytes` (cut on a char boundary, `truncated = true`), `Err` with a short message for unreadable or non-UTF-8 files; splits off frontmatter.
  - `pub struct NameIndex`, `impl NameIndex { pub fn new(entries: &[Entry]) -> Self }`
  - `pub fn resolve_wikilink(root: &Path, from: &Path, link: &str, index: &NameIndex) -> Option<PathBuf>`
  - `pub fn obsidian_url(root: &Path, file: &Path) -> String`
  - `pub fn default_root() -> PathBuf` (the Global Constraints path).

- [ ] **Step 1: Write the failing tests** (all in `vault.rs`, one temp vault per test)
  - `filename_key_reads_both_orders`: `filename_key("2026-10-13-15_26-topic")` is `Some(202610131526)`; `filename_key("13-10-2026-15_26-topic")` is `Some(202610131526)`; `filename_key("README")` is `None`.
  - `list_is_newest_first_and_hides_raw_and_dot_dirs`: vault with `20-Projects/geodb/sessions/2026-10-13-15_26-a.md`, `.../2026-10-19-09_00-b.md`, `.../sessions/raw/2026-10-19-09_00-b.md`, `.obsidian/x.md`, `home.md`; `list(root, None)` names, in order, are `b`, `a`, then `home` (file-time key) with `x` and the raw copy absent; `list(root, Some("geodb"))` has no `home`.
  - `list_does_not_follow_symlinks`: a symlink to the vault root inside the vault yields no duplicates and terminates.
  - `list_errs_when_root_is_missing`: `list(Path::new("/nope"), None)` is `Err`.
  - `products_lists_only_directories`: a file under `20-Projects/` is not a product.
  - `frontmatter_fields_and_tags_parse`: a `/wrap`-style header with `tags: [a, b]`, a quoted `time: "15:26"`, and `status: landed` fills those fields.
  - `no_frontmatter_and_unterminated_block_keep_the_whole_body`: both inputs give `front == None` and `body` equal to the input.
  - `read_note_caps_on_a_char_boundary`: a 10 000 byte note of `é` with `cap_bytes = 1001` returns `truncated == true` and valid UTF-8.
  - `read_note_reports_non_utf8`: bytes `[0xff, 0xfe]` give `Err`.
  - `wikilinks_resolve_vault_relative_note_relative_and_bare`: `[[20-Projects/geodb/README]]`, `[[raw/2026-10-19-09_00-b]]` from a summary, and `[[README|alias]]` / `[[README#Heading]]` all resolve; with two `README.md` files the one in the same product folder as `from` wins; `[[missing]]` is `None`.
  - `names_with_spaces_unicode_percent_and_hash_survive`: `[[Plan Ñandú 100%]]` resolves to `Plan Ñandú 100%.md`; `obsidian_url` for it contains `file=Plan%20%C3%91and%C3%BA%20100%25` and `vault=Obsidian`, and the `#` in `A#B.md` is `%23`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-core --profile native-dev vault::`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement the interfaces above.** Frontmatter: only the first `---` line to the next `---` line; `key: value`, strip surrounding quotes, `tags: [a, b]` split on commas. Percent-encode with a small local function (unreserved characters `A-Za-z0-9-._~` and `/` kept; everything else `%XX` on UTF-8 bytes); `file=` is the vault-relative path without the `.md` extension. Wikilink order: strip `|alias` and `#heading`; try vault-relative (append `.md` if missing); try relative to `from`'s directory; then `NameIndex` by lower-cased file stem, preferring the candidate sharing the longest directory prefix with `from`.

- [ ] **Step 4: Run to verify they pass** (same command). Expected: PASS.

- [ ] **Step 5: Commit** `git add crates/monocode-core/src/vault.rs crates/monocode-core/src/lib.rs && git commit -m "Add read-only vault listing, frontmatter and wikilink logic"`

---

### Task 2: Vault path setting and per-repo product mapping

**Files:**
- Modify: `crates/monocode-gpui/src/settings/preferences.rs` (add `pub vault_path: Option<String>`, default `None`, near `binary_paths`)
- Modify: the Settings page that renders text-path rows (see how `binary_paths` rows are built in `settings/management.rs`), adding one "Vault folder" row that stores/clears `vault_path`
- Modify: `crates/monocode-gpui/src/workspace.rs` (add `pub vault_products: HashMap<String, String>` beside `project_rail_assignments`, `HashMap::new()` in its constructor, serialized with the struct)
- Test: existing test modules in those files

**Interfaces:**
- Consumes: `vault::default_root()` from Task 1.
- Produces:
  - `impl Preferences { pub(crate) fn vault_root(&self) -> PathBuf }`: `vault_path` expanded (`~/`) when set and non-empty, else `vault::default_root()`.
  - `Workspace.vault_products`: project path → product folder name; the empty string means asked and skipped.

- [ ] **Step 1: Write the failing tests**
  - `vault_root_defaults_and_honours_the_setting`: default `Preferences` gives `vault::default_root()`; `vault_path = Some("/tmp/v".into())` gives `/tmp/v`; `Some("")` gives the default.
  - `vault_products_survive_a_save_and_restore`: a `Workspace` with `vault_products["/r/a"] = "geodb"` and `["/r/b"] = ""` restores with both entries (copy the style of the existing `restore_keeps_project_selection_and_prunes_stale_project_focus` test).

- [ ] **Step 2: Run to verify they fail**: `cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --profile native-dev -- --test-threads=1 vault_` Expected: FAIL.

- [ ] **Step 3: Implement** the field, the helper, the Settings row, and the workspace map.

- [ ] **Step 4: Run to verify they pass**, plus `settings` tests: add `settings` to the filter. Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "Add a vault folder setting and a per-repo product mapping"`

---

### Task 3: The `VaultNotes` list, chips, search and mapping prompt

**Files:**
- Create: `crates/monocode-gpui/src/vault_notes.rs`; add `mod vault_notes;` to `main.rs`
- Test: `#[cfg(test)]` module in `vault_notes.rs` using `gpui::TestAppContext` and a temp vault

**Interfaces:**
- Consumes: Task 1 `list`, `products`, `Entry`; Task 2 `vault_root`.
- Produces:
  - `pub(crate) enum Event { Close, MapProduct { project: String, product: String } }` with `impl EventEmitter<Event> for VaultNotes`
  - `pub(crate) struct VaultNotes`, `impl Focusable`, `impl Render`
  - `VaultNotes::new(root: PathBuf, cx: &mut Context<Self>) -> Self`
  - `VaultNotes::set_context(&mut self, root: PathBuf, project: String, mapping: HashMap<String, String>, cx: &mut Context<Self>)`: re-lists when `root` changed and pre-selects the chip for `mapping[project]` when it is a non-empty product.
  - Debug selectors: `vault-chip-all`, `vault-chip-<product>`, `vault-row-<index>`, `vault-search`, `vault-map-prompt`, `vault-map-skip`, `vault-missing`.

- [ ] **Step 1: Write the failing tests**
  - `lists_the_vault_newest_first_with_all_selected`: temp vault as in Task 1; after `new`, rows are in `list` order and the "all" chip is active.
  - `a_product_chip_scopes_the_list_and_roots_the_tree`: clicking `vault-chip-geodb` leaves only that product's files, and `home.md` disappears.
  - `the_mapped_product_is_preselected`: `set_context(root, "/r/a", {"/r/a": "geodb"}, cx)` activates `vault-chip-geodb`.
  - `an_unmapped_repo_shows_the_inline_prompt_and_skip_persists_empty`: no entry shows `vault-map-prompt`; clicking `vault-map-skip` emits `Event::MapProduct { project: "/r/a", product: "" }`; choosing a product emits it with that name; a mapped or skipped repo shows no prompt.
  - `search_matches_file_names_case_insensitively`: typing `SPIKE` leaves rows whose name contains `spike`.
  - `a_missing_vault_says_so_and_does_not_panic`: root `/nope` renders `vault-missing` with text naming Settings; no rows.

- [ ] **Step 2: Run to verify they fail**: `... -p monocode-gpui --profile native-dev -- --test-threads=1 vault_notes` Expected: FAIL.

- [ ] **Step 3: Implement** `VaultNotes` as a list-only surface (a folder tree is the rows grouped by relative directory; rows newest first inside each). Listing runs through `cx.background_executor()` so the UI thread never walks the vault. Reuse styling helpers from `notes.rs` for the chip row and list (read `Notes::render` for the existing look); do not import its DB code.

- [ ] **Step 4: Run to verify they pass** (same command). Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "Add the vault notes list with product chips and search"`

---

### Task 4: The note view: metadata, markdown, wikilinks, actions

**Files:**
- Modify: `crates/monocode-gpui/src/vault_notes.rs`
- Test: same module

**Interfaces:**
- Consumes: Task 1 `read_note`, `resolve_wikilink`, `NameIndex`, `obsidian_url`; the markdown pipeline used by `notes/preview.rs` (`markdown_model::parse_full_hard_breaks`, `markdown::rows`, `markdown::resolve_file_links`).
- Produces: `VaultNotes::select(&mut self, path: PathBuf, cx: &mut Context<Self>)`; `const NOTE_CAP_BYTES: usize = 512 * 1024`; selectors `vault-meta`, `vault-body`, `vault-truncated`, `vault-open-obsidian`, `vault-open-default`, `vault-back-to-summary`.

- [ ] **Step 1: Write the failing tests**
  - `selecting_a_summary_shows_its_metadata_header_and_body`: `vault-meta` text contains the topic, date and status; the raw `---` block is not rendered.
  - `a_note_without_frontmatter_has_no_header`: `vault-meta` is absent; `vault-body` present.
  - `clicking_a_wikilink_opens_the_target`: a `[[raw/<basename>]]` link selects the raw file (it is not in the tree but opens); an unresolved link renders as plain text and is not clickable.
  - `a_raw_note_offers_a_link_back_to_its_summary`: `vault-back-to-summary` present for a file under `sessions/raw/`, selecting the sibling summary of the same basename.
  - `a_huge_note_is_cut_and_says_so`: a 2 MiB note renders with `vault-truncated` visible and no more than `NOTE_CAP_BYTES` of body.
  - `an_unreadable_note_shows_an_error_row`: a non-UTF-8 file selects without panicking and shows a short error.
  - `open_in_obsidian_builds_the_url`: clicking `vault-open-obsidian` calls the URL opener with `obsidian_url(root, file)`; make the opener injectable (a closure field) so the test does not launch anything.
  - `open_with_default_app_passes_the_file_path`: clicking `vault-open-default` calls the injectable path opener with the note's absolute path.
  - `search_also_matches_topic_and_tags_of_notes_already_read`: select a note whose frontmatter has `topic: mcp-spike` and `tags: [geodb]`; searching `mcp-spike` or `geodb` then finds it, while a never-opened note with the same topic in its frontmatter is not found by that term (no whole-vault reads).

- [ ] **Step 2: Run to verify they fail**, **Step 3: Implement** (rendering reuses the `notes/preview.rs` row pipeline; links to non-vault files keep the existing `OpenPath` action), **Step 4: Run to verify they pass** (command as in Task 3).

- [ ] **Step 5: Commit** `git commit -m "Render vault notes with metadata, wikilinks and open actions"`

---

### Task 5: Live refresh

**Files:**
- Modify: `crates/monocode-gpui/src/fs_watch.rs` (add `".obsidian"` and `".trash"` to `IGNORED_DIRS`)
- Modify: `crates/monocode-gpui/src/vault_notes.rs`
- Test: `fs_watch.rs` tests and `vault_notes.rs` tests

**Interfaces:**
- Consumes: `fs_watch::watch(&[PathBuf]) -> Option<(Watch, async_channel::Receiver<()>)>`.
- Produces: `VaultNotes` holds the `Watch` for its root, re-lists on each signal after a 300 ms debounce, and re-reads the open note.

- [ ] **Step 1: Write the failing tests**
  - `fs_watch`: `obsidian_and_trash_changes_are_not_relevant` calls the existing `relevant` helper with `/v/.obsidian/workspace.json` and `/v/.trash/x.md` and expects `false`; `/v/20-Projects/a.md` stays `true`.
  - `a_new_file_appears_without_a_manual_refresh`: write a new `.md` into the temp vault, advance the executor past the debounce, and the list gains the row.
  - `a_deleted_selected_note_says_it_no_longer_exists`: select a note, delete the file, refresh; `vault-body` is replaced with a "no longer exists" message and nothing panics.
  - `a_half_written_file_does_not_break_refresh`: create an empty file, then fill it; both refreshes complete.
  - `changing_the_root_restarts_the_watch`: `set_context` with a new root lists the new vault.

- [ ] **Step 2-4:** run to fail, implement, run to pass. **Step 5: Commit** `git commit -m "Refresh the vault notes list when files change on disk"`

---

### Task 6: Wire it into the app

**Files:**
- Modify: `crates/monocode-gpui/src/shell.rs` (the `notes` field: `Option<Entity<vault_notes::VaultNotes>>`; import)
- Modify: `crates/monocode-gpui/src/shell/app_views.rs`: `open_notes` builds `VaultNotes`, subscribes to `vault_notes::Event` (`Close` closes the view; `MapProduct` writes `workspace.vault_products` and calls `persist`), and calls `set_context` with `self.workspace.preferences.vault_root()`, the current project and `vault_products`; remove the `flush` call in `leave_app_views`
- Modify: `main.rs` (`#[allow(dead_code)]` on `mod notes;`) and the now-unused `note_to_chat` in `app_views.rs` (`#[allow(dead_code)]`), only if the compiler warns
- Test: update nothing that exists; add one test beside the existing `shell.notes.is_some()` assertions

**Interfaces:**
- Consumes: Tasks 2-5.

- [ ] **Step 1: Write the failing test**
  `notes_surface_opens_the_vault_and_remembers_a_product_choice`: set `preferences.vault_path` to a temp vault, call `open_notes`, assert `vault-chip-all` is present; emit `Event::MapProduct { project, product: "geodb" }`; assert `workspace.vault_products[&project] == "geodb"` and that reopening shows the `geodb` chip active.

- [ ] **Step 2: Run to verify it fails.** **Step 3: Implement** as listed. **Step 4: Run** this test and the existing ones that touch `shell.notes` (`sidebar_list`, `shell::tests` Notes cases). Expected: PASS.

- [ ] **Step 5: Commit** `git commit -m "Point the Notes surface at the Obsidian vault"`

---

### Task 7: Verify the whole branch

- [ ] **Step 1:** Run the lint command from Global Constraints. Expected: no warnings or errors.
- [ ] **Step 2:** Run `cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-core -p monocode-gpui --profile native-dev -- --test-threads=1`. Expected: all pass.
- [ ] **Step 3:** Build the dev app and launch it against a copy of the vault (`MONOCODE_NATIVE_DATA_DIR` for the app data, Settings "Vault folder" pointing at the copy). Check, in order: tree and chips show; a `/wrap` summary shows its metadata header; a `[[raw/...]]` link opens and offers the way back; touching a file updates the list; "Open in Obsidian" opens it; a bad vault path shows the message.
- [ ] **Step 4:** Report results to Federico with what was and was not checked on screen.
