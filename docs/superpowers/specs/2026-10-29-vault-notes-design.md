# Vault Notes: the Notes surface reads the Obsidian vault

Date: 2026-10-29. Status: draft for approval. Target: the Classic layout of the
native (Rust + GPUI) app, on the `feat/vault-notes` branch of the fork.

## Intent and why

Federico keeps his working knowledge in an Obsidian vault. His agents write to
it: `/wrap` saves a session summary plus a raw transcript, and `/boot` reads the
newest summaries back at the start of a session. Monocode's own Notes section is
a separate, database-backed scratchpad. He does not want notes in two places.

So the Notes surface stops being a store and becomes a window onto the vault.
The vault files are the only copy. Monocode never keeps a second one.

He has no notes in Monocode's database today (count is 0), so nothing is
migrated.

## The vault (facts the builder can rely on)

- Root: `/Users/federicoalarcon/ObsidianVaults/Obsidian`, made configurable.
- `home.md` at the root names the last thread and last session.
- Products live at `20-Projects/<product>/` (examples: `geodb`,
  `gaia-platform`, `opportunity`, `dobk`, `automl`, `career`). Each has a
  `README.md`, a `sessions/` folder of summaries, and `sessions/raw/` with full
  transcripts that share the summary's basename.
- Session files are named `YYYY-MM-DD-HH_MM-topic.md`. Older ones are
  `DD-MM-YYYY-HH_MM-topic.md` and must not be renamed.
- Summaries start with YAML frontmatter: `date`, `time`, `topic`, `project`,
  `tool`, `model`, `branch`, `tickets`, `status`, `tags`. Bodies use Obsidian
  wikilinks such as `[[20-Projects/geodb/README]]` and `[[raw/<basename>]]`.
- Agents write to the vault while Monocode is open.

## Goals

1. **Browse.** The Notes surface lists markdown files under the vault root as a
   folder tree, newest first. Sort by frontmatter `date` and `time`, then file
   modified time as a fallback (this is the same rule `/boot` uses).
2. **Filter by product.** A strip of chips across the top: "All" plus one chip
   per folder under `20-Projects/`. "All" is the default.
3. **Pre-select the current repo's product.** The first time a repo needs it,
   ask once which product folder it belongs to and remember the answer per repo
   in Monocode's own settings. From then on that product's chip is pre-selected
   when that repo is current. Skipping the question leaves "All".
4. **Read.** Selecting a file shows it rendered as markdown. Frontmatter appears
   as a small metadata header (date, topic, status, tags), not as raw YAML.
   Wikilinks resolve to vault files and open in the same surface. Links to
   files outside the vault keep working through the existing file-opening path.
5. **Stay fresh.** The list and the open note update when files change on disk,
   with no manual refresh, because agents write while the app is open.
6. **Search.** A search box matches file name, frontmatter `topic` and `tags`.
   Matching note body text is a stretch goal, not required.
7. **Hand off.** An "Open in Obsidian" action (an `obsidian://open` link with the
   vault name and relative file path) and an "Open with default app" action.
8. **Vault location.** One Settings field, defaulting to the path above. If the
   folder is missing, the surface says so plainly and offers the Settings field.

## Non-goals

- Syncing, importing, exporting, or any second copy of a note.
- Creating notes, renaming, deleting, moving, tags, or image attachments from
  Monocode.
- Graph view, backlinks panel, canvas files, plugins, mobile, or remote
  (`remote://`) projects.
- Reading Claude Code skill configuration (`~/.claude/skills/*/vault-config.md`)
  to learn the repo-to-product mapping. That file belongs to a different tool
  and can change on its own.

## Anti-goals (a failure even if it technically works)

- Writing to the vault in the first version. An agent could be writing the same
  file; an accidental overwrite is the worst outcome.
- Slowing the app or filling memory on a resource-limited Mac by indexing or
  rendering the whole vault eagerly.
- Showing `sessions/raw/` transcripts in normal lists and drowning the summaries.

## Editing (deferred, optional)

Federico wants to read first, and edit only if that is simple. The first version
is read-only. If an edit mode is added later it must be limited to the note body
as plain text, and must refuse to save when the file's modified time changed
since it was loaded, so it cannot overwrite an agent's write. If that proves
complicated, leave it out entirely.

## Constraints

- Rust and GPUI, Classic layout. Keep the diff rebase-friendly: put the new
  surface in its own module and touch existing Notes wiring as little as possible.
  The upstream repo moves fast.
- Reuse what exists: `notes/preview.rs` renders markdown rows and calls
  `markdown::resolve_file_links`; `fs_watch::watch(&[PathBuf])` returns a watcher
  plus a coalescing change signal, with an ignore list tuned for git checkouts
  (the vault needs its own: `.obsidian`, `.git`, `.trash`).
- Files can be large. Cap what is rendered for one note and say when it was cut.
- Lint is `cargo clippy -- -D warnings` on `monocode-core` and `monocode-gpui`,
  and the full GUI suite must stay green.

## Decisions Federico made

- The vault is the only copy; no notes in two places.
- Show the whole vault by default, with a filter by product as an option.
- Read and edit, but mainly read; leave edit out if it is complicated.
- The current repo's product is chosen by asking once and remembering it.

## Calls made by the author (not yet confirmed)

- The first version is read-only, with "Open in Obsidian" as the way to edit.
- `sessions/raw/` is hidden from lists by default and reachable from a summary's
  `[[raw/...]]` link.
- The mapping from repo to product folder is stored in Monocode's per-repo
  settings, not in the vault.
- The existing database-backed Notes code stays in the tree but is no longer
  reachable from the Notes entry. Delete it only if the compiler's dead-code
  warnings force that.
- Body-text search is a stretch goal.

## Calls added after a reviewer's read (author's calls, not Federico's)

- **Repo key and storage.** The key is the project path, the same key the app
  already uses for per-project state. Persist the mapping as a
  `HashMap<String, String>` next to `project_rail_assignments` and
  `project_appearance` in the saved workspace state, so no new store is needed.
  An empty string means "asked and skipped". Worktrees and moved checkouts that
  have a different path simply ask again.
- **The "ask once" prompt.** No modal. When Notes opens and the current repo has
  no entry, the chip strip shows a single inline line ("Which product is
  `<repo>`?") with a dropdown of the product folders and a "Skip" button. Skip
  stores the empty string and it never asks again for that repo. The mapping can
  be changed later from the same strip.
- **What the chips cover.** A product chip shows the whole `20-Projects/<product>/`
  folder (README, sessions, anything else), and the tree is rooted at that
  folder. "All" roots the tree at the vault and is the only view that shows
  files outside `20-Projects/` (such as `home.md`).
- **Newest first, without scanning everything.** Derive the sort key from the
  file name first (`YYYY-MM-DD-HH_MM` or legacy `DD-MM-YYYY-HH_MM`), fall back to
  modified time, and read frontmatter only for the files on screen or open.
  Listing paths is cheap; reading file contents for the whole vault is not
  done. Files without frontmatter simply show no metadata header.
- **Search.** File name matches immediately. `topic` and `tags` match only for
  files whose frontmatter has already been read; do not read the whole vault to
  satisfy a search.
- **Wikilinks.** Support vault-relative (`20-Projects/geodb/README`),
  note-relative (`raw/<basename>`) and bare-name links, resolved against a
  name index built from file paths only. Strip `#heading` and `|alias` suffixes
  before resolving. An unresolved or ambiguous link renders as plain text; for
  ambiguity, prefer the match closest to the current note.
- **`sessions/raw/`.** Hidden from the tree and from search. Opening one through
  a link works and uses the same render cap; the note view offers the summary it
  belongs to as a link back.
- **Renderer scope.** Use the existing markdown renderer as it is. Obsidian-only
  syntax (callouts, `![[embeds]]`, `==highlights==`) degrades to plain text.
  Extending the renderer is out of scope.
- **Watcher and odd files.** Watch the vault root recursively through
  `fs_watch` with a vault-specific ignore list. Do not follow symlinks. A file
  that is not valid UTF-8 or cannot be read shows a short error row instead of
  failing the list.

## Left to the builder

Module layout and names; the exact tree and chip visuals within the existing
design language; how frontmatter is parsed; debounce timing for refresh; how
much of a large note to render; test fixtures (use a temporary vault folder).

## What done looks like

With the dev build pointed at a copy of the vault, opening Notes shows the tree
and chips; clicking a `/wrap` summary renders it with its metadata header;
following a wikilink opens the target; touching a file on disk updates the list
within a second or two; "Open in Obsidian" launches it; the whole GUI suite and
clippy pass.
