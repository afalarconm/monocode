//! Read-only access to an Obsidian vault: listing, sorting, frontmatter,
//! wikilinks and reading. Nothing here ever writes to the vault, and nothing
//! reads a file's contents except `read_note`.
use std::path::{Path, PathBuf};

/// One markdown file in the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    /// Relative to the vault root.
    pub rel: PathBuf,
    /// The file name without `.md`.
    pub name: String,
    /// `YYYYMMDDHHMM`: from the file name when it carries a date, else the
    /// file's modified time. Higher is newer.
    pub sort_key: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub date: Option<String>,
    pub time: Option<String>,
    pub topic: Option<String>,
    pub project: Option<String>,
    pub status: Option<String>,
    pub tool: Option<String>,
    pub model: Option<String>,
    pub branch: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub front: Option<Frontmatter>,
    pub body: String,
    pub truncated: bool,
}

/// Lower-cased file stem to every path that carries it. Built from paths
/// only, so it never reads a file.
pub struct NameIndex {
    by_stem: std::collections::HashMap<String, Vec<PathBuf>>,
}

pub fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("/Users/federicoalarcon"), PathBuf::from)
        .join("ObsidianVaults/Obsidian")
}

/// `YYYY-MM-DD-HH_MM-...` or the legacy `DD-MM-YYYY-HH_MM-...`, as `YYYYMMDDHHMM`.
pub fn filename_key(name: &str) -> Option<u64> {
    let b = name.as_bytes();
    if b.len() < 16 || b[10] != b'-' || b[13] != b'_' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u64> {
        let part = name.get(from..to)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day) = if b[4] == b'-' && b[7] == b'-' {
        (num(0, 4)?, num(5, 7)?, num(8, 10)?)
    } else if b[2] == b'-' && b[5] == b'-' {
        (num(6, 10)?, num(3, 5)?, num(0, 2)?)
    } else {
        return None;
    };
    Some((((year * 100 + month) * 100 + day) * 100 + num(11, 13)?) * 100 + num(14, 16)?)
}

fn modified_key(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|time| {
            chrono::DateTime::<chrono::Utc>::from(time)
                .format("%Y%m%d%H%M")
                .to_string()
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Directory names directly under `20-Projects/`.
pub fn products(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("20-Projects")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Every `.md` file under the vault (or under one product), newest first.
/// Hidden directories, `sessions/raw/` and symlinks are skipped. Only a root
/// that cannot be read is an error. File contents are never read.
pub fn list(root: &Path, product: Option<&str>) -> Result<Vec<Entry>, String> {
    std::fs::read_dir(root).map_err(|e| format!("Cannot read {}: {e}", root.display()))?;
    let base = match product {
        Some(product) => root.join("20-Projects").join(product),
        None => root.to_path_buf(),
    };
    let mut entries = Vec::new();
    let mut stack = vec![base];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in read.flatten() {
            let Ok(kind) = item.file_type() else { continue };
            let path = item.path();
            let file_name = item.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if kind.is_dir() {
                let in_sessions = dir.file_name().is_some_and(|n| n == "sessions");
                if !name.starts_with('.') && !(in_sessions && name == "raw") {
                    stack.push(path);
                }
            } else if kind.is_file()
                && path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            {
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let sort_key = filename_key(stem).unwrap_or_else(|| modified_key(&path));
                entries.push(Entry {
                    rel: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                    name: stem.to_owned(),
                    path,
                    sort_key,
                });
            }
        }
    }
    entries.sort_by(|a, b| b.sort_key.cmp(&a.sort_key).then_with(|| a.rel.cmp(&b.rel)));
    Ok(entries)
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|v| v.strip_suffix(quote))
        {
            return inner.to_owned();
        }
    }
    value.to_owned()
}

/// A leading `---` block, if it is closed. Anything else is all body.
fn split_frontmatter(text: &str) -> (Option<Frontmatter>, &str) {
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, text);
    }
    let mut front = Frontmatter::default();
    let mut used = 4;
    for line in lines {
        used += line.len();
        if line.trim_end() == "---" {
            return (Some(front), text.get(used..).unwrap_or(""));
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let text = unquote(value);
        let field = match key.trim() {
            "date" => &mut front.date,
            "time" => &mut front.time,
            "topic" => &mut front.topic,
            "project" => &mut front.project,
            "status" => &mut front.status,
            "tool" => &mut front.tool,
            "model" => &mut front.model,
            "branch" => &mut front.branch,
            "tags" => {
                front.tags = text
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .split(',')
                    .map(unquote)
                    .filter(|tag| !tag.is_empty())
                    .collect();
                continue;
            }
            _ => continue,
        };
        *field = (!text.is_empty()).then_some(text);
    }
    (None, text)
}

/// Reads at most `cap_bytes`, cut on a character boundary.
pub fn read_note(path: &Path, cap_bytes: usize) -> Result<Note, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open note: {e}"))?;
    let mut bytes = Vec::new();
    file.take(cap_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Cannot read note: {e}"))?;
    let truncated = bytes.len() > cap_bytes;
    bytes.truncate(cap_bytes);
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        // The cut landed inside a character; anything else is a bad file.
        Err(e) if truncated && e.error_len().is_none() => {
            std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return Err("This note is not valid UTF-8 text.".into()),
    };
    let (front, body) = split_frontmatter(text);
    Ok(Note {
        front,
        body: body.to_owned(),
        truncated,
    })
}

impl NameIndex {
    pub fn new(entries: &[Entry]) -> Self {
        let mut by_stem: std::collections::HashMap<String, Vec<PathBuf>> = Default::default();
        for entry in entries {
            by_stem
                .entry(entry.name.to_lowercase())
                .or_default()
                .push(entry.path.clone());
        }
        for paths in by_stem.values_mut() {
            paths.sort();
        }
        Self { by_stem }
    }
}

/// True when `path` really lies under `root`, following symlinks.
fn inside(root: &Path, path: &Path) -> bool {
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(root), Ok(path)) => path.starts_with(root),
        _ => false,
    }
}

fn with_md(link: &str) -> String {
    if link.ends_with(".md") {
        link.to_owned()
    } else {
        format!("{link}.md")
    }
}

/// Vault-relative, then note-relative, then a bare name. `|alias` and
/// `#heading` are dropped first. An ambiguous bare name resolves to the file
/// sharing the longest directory prefix with `from`.
pub fn resolve_wikilink(
    root: &Path,
    from: &Path,
    link: &str,
    index: &NameIndex,
) -> Option<PathBuf> {
    let target = link.split('|').next()?.split('#').next()?.trim();
    if target.is_empty()
        || Path::new(target).is_absolute()
        || Path::new(target)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let file = with_md(target);
    let vault_relative = root.join(&file);
    if vault_relative.is_file() && inside(root, &vault_relative) {
        return Some(vault_relative);
    }
    if let Some(dir) = from.parent() {
        let note_relative = dir.join(&file);
        if note_relative.is_file() && inside(root, &note_relative) {
            return Some(note_relative);
        }
    }
    let stem = Path::new(target).file_name()?.to_str()?.to_lowercase();
    let from_dir = from.parent().unwrap_or(root);
    let shared = |candidate: &PathBuf| {
        candidate
            .ancestors()
            .skip(1)
            .filter(|dir| from_dir.starts_with(dir))
            .count()
    };
    index
        .by_stem
        .get(&stem)?
        .iter()
        .max_by(|a, b| shared(a).cmp(&shared(b)).then_with(|| b.cmp(a)))
        .filter(|path| inside(root, path))
        .cloned()
}

fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A `file://` link with every byte outside the unreserved set escaped, so
/// names with spaces, unicode or `%` survive the markdown link round trip.
pub fn file_url(path: &Path) -> String {
    format!("file://{}", percent_encode(&path.to_string_lossy()))
}

/// `obsidian://open` for a file inside the vault. The vault name is the
/// root folder's name.
pub fn obsidian_url(root: &Path, file: &Path) -> String {
    let vault = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Obsidian");
    let rel = file.strip_prefix(root).unwrap_or(file);
    let rel = rel.with_extension("");
    let rel = rel
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    format!(
        "obsidian://open?vault={}&file={}",
        percent_encode(vault),
        percent_encode(&rel)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        path
    }
    /// A file with no date in its name sorts by modified time; pin that time
    /// far in the past so the order does not depend on when the test runs.
    fn make_old(path: &Path) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_577_836_800))
            .unwrap();
    }

    #[test]
    fn filename_key_reads_both_orders() {
        assert_eq!(filename_key("2026-10-13-15_26-topic"), Some(202610131526));
        assert_eq!(filename_key("13-10-2026-15_26-topic"), Some(202610131526));
        assert_eq!(filename_key("README"), None);
    }

    #[test]
    fn list_is_newest_first_and_hides_raw_and_dot_dirs() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        write(
            root,
            "20-Projects/geodb/sessions/2026-10-13-15_26-a.md",
            "a",
        );
        write(
            root,
            "20-Projects/geodb/sessions/2026-10-19-09_00-b.md",
            "b",
        );
        write(
            root,
            "20-Projects/geodb/sessions/raw/2026-10-19-09_00-b.md",
            "raw",
        );
        write(root, ".obsidian/x.md", "x");
        make_old(&write(root, "home.md", "h"));
        let names: Vec<_> = list(root, None)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["2026-10-19-09_00-b", "2026-10-13-15_26-a", "home"]);
        let scoped = list(root, Some("geodb")).unwrap();
        assert_eq!(scoped.len(), 2);
        assert!(scoped.iter().all(|e| e.name != "home"));
    }

    #[cfg(unix)]
    #[test]
    fn list_does_not_follow_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        write(root, "home.md", "h");
        std::os::unix::fs::symlink(root, root.join("loop")).unwrap();
        let entries = list(root, None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "home");
    }

    #[test]
    fn list_errs_when_root_is_missing() {
        assert!(list(Path::new("/nope/not/here"), None).is_err());
    }

    #[test]
    fn products_lists_only_directories() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        write(root, "20-Projects/opportunity/y.md", "y");
        write(root, "20-Projects/geodb/x.md", "x");
        write(root, "20-Projects/stray.md", "s");
        assert_eq!(products(root), ["geodb", "opportunity"]);
    }

    #[test]
    fn frontmatter_fields_and_tags_parse() {
        let t = tempfile::tempdir().unwrap();
        let path = write(
            t.path(),
            "n.md",
            "---\ndate: 2026-10-13\ntime: \"15:26\"\ntopic: geodb-mcp-spike\nproject: geodb\ntool: claude-code\nmodel: unknown\nbranch: develop\ntickets: []\nstatus: landed\ntags: [geodb, mcp, spike]\n---\n# Session\nbody\n",
        );
        let note = read_note(&path, 1_000_000).unwrap();
        let front = note.front.unwrap();
        assert_eq!(front.date.as_deref(), Some("2026-10-13"));
        assert_eq!(front.time.as_deref(), Some("15:26"));
        assert_eq!(front.topic.as_deref(), Some("geodb-mcp-spike"));
        assert_eq!(front.status.as_deref(), Some("landed"));
        assert_eq!(front.tags, ["geodb", "mcp", "spike"]);
        assert!(note.body.starts_with("# Session"));
        assert!(!note.truncated);
    }

    #[test]
    fn no_frontmatter_and_unterminated_block_keep_the_whole_body() {
        let t = tempfile::tempdir().unwrap();
        for text in ["# Just a note\n", "---\ntopic: x\nno end\n"] {
            let path = write(t.path(), "n.md", text);
            let note = read_note(&path, 1_000_000).unwrap();
            assert_eq!(note.front, None);
            assert_eq!(note.body, text);
        }
    }

    #[test]
    fn read_note_caps_on_a_char_boundary() {
        let t = tempfile::tempdir().unwrap();
        let path = write(t.path(), "big.md", &"é".repeat(5_000));
        let note = read_note(&path, 1_001).unwrap();
        assert!(note.truncated);
        assert_eq!(note.body.len(), 1_000);
        assert!(note.body.chars().all(|c| c == 'é'));
    }

    #[test]
    fn read_note_reports_non_utf8() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("bad.md");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(read_note(&path, 1_000).is_err());
    }

    #[test]
    fn wikilinks_resolve_vault_relative_note_relative_and_bare() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let geodb = write(root, "20-Projects/geodb/README.md", "g");
        let opp = write(root, "20-Projects/opportunity/README.md", "o");
        let summary = write(
            root,
            "20-Projects/geodb/sessions/2026-10-19-09_00-b.md",
            "s",
        );
        let raw = write(
            root,
            "20-Projects/geodb/sessions/raw/2026-10-19-09_00-b.md",
            "r",
        );
        let index = NameIndex::new(&list(root, None).unwrap());
        let r = |from: &Path, link: &str| resolve_wikilink(root, from, link, &index);
        assert_eq!(r(&summary, "20-Projects/geodb/README"), Some(geodb.clone()));
        assert_eq!(r(&summary, "raw/2026-10-19-09_00-b"), Some(raw));
        // Two READMEs exist: the one in the note's own product wins.
        assert_eq!(r(&summary, "README|alias"), Some(geodb.clone()));
        assert_eq!(r(&summary, "README#Heading"), Some(geodb));
        assert_eq!(r(&opp, "README"), Some(opp.clone()));
        assert_eq!(r(&summary, "missing"), None);
        assert_eq!(r(&summary, "../../../../outside"), None);
    }

    #[test]
    fn wikilinks_cannot_leave_the_vault() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Obsidian");
        let inside = write(&root, "inside.md", "i");
        let outside = write(t.path(), "outside.md", "o");
        let index = NameIndex::new(&list(&root, None).unwrap());
        let from = root.join("home.md");
        // An absolute target replaces the vault root when joined.
        let absolute = outside.with_extension("").to_string_lossy().into_owned();
        assert_eq!(resolve_wikilink(&root, &from, &absolute, &index), None);
        assert_eq!(
            resolve_wikilink(&root, &from, "inside", &index),
            Some(inside)
        );
        #[cfg(unix)]
        {
            let elsewhere = t.path().join("elsewhere");
            write(&elsewhere, "secret.md", "s");
            std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
            assert_eq!(resolve_wikilink(&root, &from, "link/secret", &index), None);
        }
    }

    #[test]
    fn file_url_encodes_spaces_unicode_and_percent() {
        assert_eq!(
            file_url(Path::new("/v/Plan Ñandú 100%.md")),
            "file:///v/Plan%20%C3%91and%C3%BA%20100%25.md"
        );
    }

    #[test]
    fn names_with_spaces_unicode_percent_and_hash_survive() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("Obsidian");
        let plan = write(&root, "Plan Ñandú 100%.md", "p");
        let hash = write(&root, "A#B.md", "h");
        let index = NameIndex::new(&list(&root, None).unwrap());
        assert_eq!(
            resolve_wikilink(&root, &root.join("home.md"), "Plan Ñandú 100%", &index),
            Some(plan.clone())
        );
        let url = obsidian_url(&root, &plan);
        assert!(url.starts_with("obsidian://open?"));
        assert!(url.contains("vault=Obsidian"));
        assert!(url.contains("file=Plan%20%C3%91and%C3%BA%20100%25"));
        assert!(obsidian_url(&root, &hash).contains("file=A%23B"));
    }
}
