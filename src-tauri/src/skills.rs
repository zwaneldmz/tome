//! Skills catalog: recursively scans a skills root for `SKILL.md` files,
//! parses each one's YAML frontmatter, and serves a sorted `list` plus a
//! per-skill `read` (skill metadata + markdown body).
//!
//! The skills themselves are already vendored: dev builds resolve the root
//! to the `.agents/skills/` sibling of the working directory, packaged
//! builds to `<app_data_dir>/skills`. [`default_root`] owns that split; every
//! other function here takes an explicit `&Path` root so the parser and
//! scanner are unit-testable without a live `AppHandle` — the same "testable
//! core + thin AppHandle wrapper" split `events.rs`/`fs.rs` use, with
//! `ipc::skills` as the thin command wrapper.
//!
//! Frontmatter is the minimal YAML subset these skills actually use: a
//! leading `---` fence, then `name:`/`description:` keys (every other key —
//! `disable-model-invocation`, `argument-hint`, ... — is ignored), then a
//! closing `---` fence. `description` additionally supports YAML block
//! scalars (`>-`, `>`, `>+`, `|`, `|-`, `|+`), where the value is the
//! following indented lines joined and whitespace-collapsed into one string
//! (`orchestration/SKILL.md` is the worked example). The body returned by
//! [`read`] is everything after the closing fence (whole file when there is
//! no frontmatter), leading newline trimmed.

use std::path::{Path, PathBuf};

use tauri::Manager;

/// One skill entry surfaced to the renderer. Serialized directly (serde) for
/// the `skills:list` wire reply, so the field order/shape here IS the JSON
/// shape; `skills:read` re-uses `name`/`description` and adds `body`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Skill {
    /// Frontmatter `name`, falling back to the skill's directory name.
    pub name: String,
    /// Frontmatter `description`; `""` when absent or unparseable.
    pub description: String,
    /// Path relative to the skills root, WITHOUT the trailing `SKILL.md`
    /// (e.g. `orchestration`, `mattpocock/tdd`).
    pub rel: String,
}

/// Recursively scans `root` for files named exactly `SKILL.md` and returns
/// their parsed [`Skill`]s, sorted by `name`. Never errors: a missing root
/// (or any unreadable directory along the way) just yields nothing.
pub fn list(root: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    scan(root, root, &mut out);
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Looks up a skill by `name` and returns it plus its markdown body (the
/// text AFTER the frontmatter fence, leading newline trimmed — or the whole
/// file when it has no frontmatter). `None` if no such skill exists.
pub fn read(root: &Path, name: &str) -> Option<(Skill, String)> {
    for skill in list(root) {
        if skill.name == name {
            let path = root.join(&skill.rel).join("SKILL.md");
            let content = std::fs::read_to_string(path).ok()?;
            return Some((skill, body_of(&content)));
        }
    }
    None
}

/// Resolves the skills root for the running app. Dev mode first: if a
/// `.agents/skills` sibling of [`std::env::current_dir`] exists, use it (the
/// vendored skills). Otherwise use `<app_data_dir>/skills`, creating it on
/// demand. `None` only when `app_data_dir` itself is unresolvable AND the
/// `.agents/skills` fallback is not present.
pub fn default_root(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        let dev = cwd.join(".agents").join("skills");
        if dev.is_dir() {
            return Some(dev);
        }
    }
    let dir = app.path().app_data_dir().ok()?.join("skills");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Depth-first scan for `SKILL.md`. Skips `.git`/`node_modules`, does not
/// follow symlinks, and silently swallows any unreadable directory.
fn scan(dir: &Path, root: &Path, out: &mut Vec<Skill>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".git" || name == "node_modules" {
                continue;
            }
            scan(&path, root, out);
        } else if file_type.is_file() && entry.file_name().to_string_lossy() == "SKILL.md" {
            if let Some(skill) = load_skill(root, &path) {
                out.push(skill);
            }
        }
    }
}

/// Reads and parses one `SKILL.md` into a [`Skill`] (body discarded — the
/// list only needs metadata). `rel` is the skill's directory, relative to
/// `root`.
fn load_skill(root: &Path, path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    let rel = dir.strip_prefix(root).ok()?.to_string_lossy().into_owned();
    Some(parse_frontmatter(&content, &rel))
}

/// Splits `s` at its first `\n`, returning `(line, remainder)`. The line
/// keeps any trailing `\r` (callers `trim`/strip it); `None` when `s` has no
/// newline.
fn split_line(s: &str) -> Option<(&str, &str)> {
    let idx = s.find('\n')?;
    Some((&s[..idx], &s[idx + 1..]))
}

/// Splits a SKILL.md document into its frontmatter lines (the fence contents,
/// with the opening/closing `---` markers already removed, and trailing `\r`
/// stripped) and the body that follows the closing fence. `None` when the
/// file does not begin with a `---` fence (or has an opening fence with no
/// closing one) — the whole file is then body.
fn parse_document(content: &str) -> Option<(Vec<&str>, &str)> {
    let (first, mut rest) = split_line(content)?;
    if first.trim() != "---" {
        return None;
    }
    let mut fm = Vec::new();
    loop {
        match split_line(rest) {
            Some((line, next)) => {
                if line.trim() == "---" {
                    return Some((fm, trim_body(next)));
                }
                fm.push(line.strip_suffix('\r').unwrap_or(line));
                rest = next;
            }
            None => {
                // Final line with no trailing newline.
                if rest.trim() == "---" {
                    return Some((fm, ""));
                }
                return None;
            }
        }
    }
}

/// Parses `name`/`description` out of a document's frontmatter. `dir_rel` is
/// the skill's directory relative to the root; its last segment is the
/// fallback `name` when the `name:` key is missing.
fn parse_frontmatter(content: &str, dir_rel: &str) -> Skill {
    let mut name = last_segment(dir_rel).to_string();
    let mut description = String::new();

    if let Some((fm, _)) = parse_document(content) {
        let mut i = 0;
        while i < fm.len() {
            let line = fm[i];
            // Indented lines are block-scalar continuations, not keys.
            if line.starts_with(' ') || line.starts_with('\t') {
                i += 1;
                continue;
            }
            let Some((key, val)) = line.split_once(':') else {
                i += 1;
                continue;
            };
            match key.trim() {
                "name" => name = strip_quotes(val.trim()),
                "description" => {
                    let (desc, consumed) = description_value(&fm, i, val.trim());
                    description = desc;
                    i += consumed;
                }
                _ => {}
            }
            i += 1;
        }
    }

    Skill { name, description, rel: dir_rel.to_string() }
}

/// The body a `read` should return: the post-fence body when the document has
/// frontmatter, the whole file otherwise.
fn body_of(content: &str) -> String {
    match parse_document(content) {
        Some((_, body)) => body.to_string(),
        None => content.to_string(),
    }
}

/// Strips the leading newline(s) left after the closing `---` fence and any
/// trailing whitespace, so the body reads clean (`"hello world"` rather than
/// `"\nhello world\n"`).
fn trim_body(body: &str) -> &str {
    body.trim_start_matches(|c: char| c == '\n' || c == '\r')
        .trim_end_matches(|c: char| c == '\n' || c == '\r' || c == ' ' || c == '\t')
}

/// The last path segment of a skill's directory — `mattpocock/tdd` -> `tdd`,
/// `orchestration` -> `orchestration`.
fn last_segment(rel: &str) -> &str {
    Path::new(rel)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
}

/// Strips one pair of matching single/double quotes from an already-trimmed
/// value; passes anything else through unchanged.
fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Resolves a `description:` value from frontmatter line `idx`. Inline values
/// are quote-stripped as-is; a block-scalar indicator (`>-`, `>`, `>+`, `|`,
/// `|-`, `|+`) or an empty value means the real text is the following
/// indented lines, joined and whitespace-collapsed. Returns the description
/// and how many continuation lines were consumed (so the caller skips them).
fn description_value(lines: &[&str], idx: usize, raw: &str) -> (String, usize) {
    let block = raw.is_empty() || matches!(raw, ">" | ">-" | ">+" | "|" | "|-" | "|+");
    if !block {
        return (strip_quotes(raw), 0);
    }
    let mut words: Vec<&str> = Vec::new();
    let mut consumed = 0;
    for line in &lines[idx + 1..] {
        if line.starts_with(' ') || line.starts_with('\t') {
            words.extend(line.split_whitespace());
            consumed += 1;
        } else {
            break;
        }
    }
    (words.join(" "), consumed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_skill(dir: &Path, name: &str, content: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn list_finds_and_sorts_skills() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "foo", "---\nname: alpha\ndescription: a\n---\nbody");
        write_skill(dir.path(), "bar", "---\nname: beta\ndescription: b\n---\nbody");

        let skills = list(dir.path());
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "alpha");
        assert_eq!(skills[1].name, "beta");
        // Sorted by name, but each keeps its own directory-relative path.
        assert_eq!(skills[0].rel, "foo");
        assert_eq!(skills[1].rel, "bar");
    }

    #[test]
    fn read_returns_body_without_frontmatter() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "foo",
            "---\nname: alpha\ndescription: does things\n---\nhello world\n",
        );

        let (skill, body) = read(dir.path(), "alpha").unwrap();
        assert_eq!(body, "hello world");
        assert_eq!(skill.description, "does things");
        assert_eq!(skill.rel, "foo");
    }

    #[test]
    fn folded_description_is_flattened() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "foo",
            "---\nname: alpha\ndescription: >-\n  first line\n  second line\n---\nbody",
        );

        let skills = list(dir.path());
        assert_eq!(skills[0].description, "first line second line");
    }

    #[test]
    fn missing_frontmatter_falls_back_to_dir_name() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "nofm", "just a body\nwith no frontmatter\n");

        let skills = list(dir.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "nofm");
        assert_eq!(skills[0].description, "");
    }

    #[test]
    fn list_of_missing_root_is_empty() {
        let dir = tempdir().unwrap();
        assert_eq!(list(&dir.path().join("absent")), Vec::<Skill>::new());
    }

    #[test]
    fn read_missing_skill_returns_none() {
        let dir = tempdir().unwrap();
        assert_eq!(read(dir.path(), "nope"), None);
    }
}
