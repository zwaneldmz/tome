//! `git` exec wrapper backing the `git_info`, `git_branches`,
//! `git_checkout`, `git_log`, `git_commit`, `git_diff` commands. Ports
//! `src/main/index.js`'s git handlers (~lines 1011-1093) and its `git()`
//! helper (~line 307) — a plain `git` subprocess call with a 10s timeout
//! (the plan is explicit: keep shelling out, do not switch to `git2`).
//!
//! None of these confine `dir`. That matches the Electron original exactly
//! (verified by reading every `git:*` handler body: none of them call
//! `confinedRealPath`) — see `fs.rs`'s doc comment for the fuller citation
//! of *why* index.js draws that line (the "file-open confinement" comment
//! above its `openFolders` declaration). `crate::confine` exists, real and
//! tested, for the handlers that actually do confine (none of them in this
//! slice).

use std::time::Duration;

use serde_json::{json, Value};

const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Ports index.js's `const git = (dir, args) => new Promise(...)`:
/// `git -C <dir> <args>`, 10s timeout. `kill_on_drop(true)` makes the
/// timeout path actually kill the child instead of orphaning it — Node's
/// `execFile` timeout actively SIGTERMs, and `tokio::process::Command`
/// does *not* kill on drop by default, so this is required for parity,
/// not just tidiness. On failure, prefers `stderr` (trimmed) over a
/// generic message, matching the original's `(stderr || err.message).
/// trim()`; on success, resolves the raw (untrimmed) stdout — callers
/// trim where the original callers did, and not otherwise.
async fn git(dir: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args).kill_on_drop(true);

    let output = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        Err(format!(
            "git {} exited with {}",
            args.join(" "),
            output.status
        ))
    } else {
        Err(trimmed.to_string())
    }
}

/// `git:info`. Swallows every failure (not a repo, `git` missing, ...)
/// into `{ repo: false }`, same as the original's outer try/catch; the
/// inner ahead/behind lookup has its own narrower catch for "no upstream
/// configured", same shape (falls back to `0`/`0`).
pub async fn info(dir: &str) -> Value {
    async fn inner(dir: &str) -> Result<Value, String> {
        let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await?
            .trim()
            .to_string();

        let mut added = 0i64;
        let mut modified = 0i64;
        let mut deleted = 0i64;
        let status = git(dir, &["status", "--porcelain"]).await?;
        for line in status.split('\n') {
            if line.is_empty() {
                continue;
            }
            let mut chars = line.chars();
            let x = chars.next();
            let y = chars.next();
            if x == Some('?') || x == Some('A') {
                added += 1;
            } else if x == Some('D') || y == Some('D') {
                deleted += 1;
            } else {
                modified += 1;
            }
        }

        let mut ahead = 0i64;
        let mut behind = 0i64;
        if let Ok(ab) = git(dir, &["rev-list", "--left-right", "--count", "@{u}...HEAD"]).await {
            let parts: Vec<&str> = ab.split_whitespace().collect();
            behind = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            ahead = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        }

        Ok(json!({
            "repo": true,
            "branch": branch,
            "added": added,
            "modified": modified,
            "deleted": deleted,
            "ahead": ahead,
            "behind": behind,
        }))
    }

    inner(dir)
        .await
        .unwrap_or_else(|_| json!({ "repo": false }))
}

/// `git:branches`. No try/catch in the original — a `git()` failure
/// propagates as a rejection, ported here as `Err`.
pub async fn branches(dir: &str) -> Result<Value, String> {
    let out = git(
        dir,
        &[
            "branch",
            "--sort=-committerdate",
            "--format=%(refname:short)",
        ],
    )
    .await?;
    let list: Vec<&str> = out.split('\n').filter(|s| !s.is_empty()).collect();
    Ok(json!(list))
}

/// `git:checkout`. Never propagates an `Err` — the original's try/catch
/// always resolves with `{ ok, error? }`.
pub async fn checkout(dir: &str, branch: &str, create: bool) -> Value {
    let args: Vec<&str> = if create {
        vec!["checkout", "-b", branch]
    } else {
        vec!["checkout", branch]
    };
    match git(dir, &args).await {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

const LOG_SEP: char = '\u{1f}';

/// `git:log`. `limit` mirrors `` `-${limit || 250}` `` — JS's `||` treats
/// `0` as falsy too, so an explicit "0" still falls back to 250, not to
/// "no limit"; `filter(|&n| n != 0)` reproduces that.
pub async fn log(dir: &str, limit: Option<u32>) -> Result<Value, String> {
    let n = limit.filter(|&n| n != 0).unwrap_or(250);
    let pretty =
        format!("--pretty=format:%H{LOG_SEP}%h{LOG_SEP}%an{LOG_SEP}%ad{LOG_SEP}%D{LOG_SEP}%s");
    let out = git(
        dir,
        &[
            "log",
            &format!("-{n}"),
            "--date=format-local:%Y-%m-%d %H:%M",
            &pretty,
        ],
    )
    .await?;

    let entries: Vec<Value> = out
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let mut parts = line.split(LOG_SEP);
            let hash = parts.next().unwrap_or("");
            let short = parts.next().unwrap_or("");
            let author = parts.next().unwrap_or("");
            let date = parts.next().unwrap_or("");
            let refs = parts.next().unwrap_or("");
            let subject = parts.next().unwrap_or("");
            let refs_list: Vec<&str> = if refs.is_empty() {
                vec![]
            } else {
                refs.split(", ").filter(|s| !s.is_empty()).collect()
            };
            json!({
                "hash": hash,
                "short": short,
                "author": author,
                "date": date,
                "refs": refs_list,
                "subject": subject,
            })
        })
        .collect();
    Ok(json!(entries))
}

/// `git:commit`. Only the root-commit diff fallback is caught (mirrors the
/// original's inner-only try/catch) — a failure in the initial `git show`
/// propagates as `Err`, uncaught, same as the original.
pub async fn commit(dir: &str, hash: &str) -> Result<Value, String> {
    let body = git(dir, &["show", "-s", "--format=%B", hash])
        .await?
        .trim()
        .to_string();

    let raw = match git(
        dir,
        &["diff", "--name-status", "-M", &format!("{hash}^"), hash],
    )
    .await
    {
        Ok(s) => s,
        // root commit has no parent — diff-tree against nothing instead
        Err(_) => {
            git(
                dir,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-status",
                    "-r",
                    "-M",
                    "--root",
                    hash,
                ],
            )
            .await?
        }
    };

    let files: Vec<Value> = raw
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            let status = parts
                .first()
                .and_then(|s| s.chars().next())
                .map(|c| c.to_string())
                .unwrap_or_default();
            let path = parts.last().copied().unwrap_or("");
            json!({ "status": status, "path": path })
        })
        .collect();

    Ok(json!({ "body": body, "files": files }))
}

/// `git:diff`. Only the first attempt is caught, with a fallback to `git
/// show` for a root commit (no `^` parent to diff against) — a failure in
/// *that* fallback propagates as `Err`, uncaught, same as the original.
pub async fn diff(dir: &str, hash: &str, file: &str) -> Result<Value, String> {
    let text = match git(dir, &["diff", &format!("{hash}^"), hash, "--", file]).await {
        Ok(s) => s,
        Err(_) => git(dir, &["show", "--format=", hash, "--", file]).await?,
    };
    Ok(json!(text))
}

/// `git:status`. Decodes `git status --porcelain` into a per-file list for
/// the commit UI — each line's XY code pair (index/worktree) plus the raw
/// path with the single separator space stripped. Renames keep their
/// `old -> new` form verbatim, same as the porcelain output.
pub async fn status(dir: &str) -> Result<Value, String> {
    let out = git(dir, &["status", "--porcelain"]).await?;
    let files: Vec<Value> = out
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let mut chars = line.chars();
            let x = chars.next().map(|c| c.to_string()).unwrap_or_default();
            let y = chars.next().map(|c| c.to_string()).unwrap_or_default();
            // `XY PATH` — drop the two code chars and the one separator space.
            let path = line.get(3..).unwrap_or("");
            json!({ "x": x, "y": y, "path": path })
        })
        .collect();
    Ok(json!({ "files": files }))
}

/// `git:stage`. `git add -A` when no paths are given, `git add -- <paths>`
/// otherwise. Never propagates an `Err` — mirrors `checkout`'s `{ ok,
/// error? }` shape.
pub async fn stage(dir: &str, paths: Option<Vec<String>>) -> Result<Value, String> {
    let result = match paths {
        Some(p) if !p.is_empty() => {
            let mut args = vec!["add", "--"];
            args.extend(p.iter().map(|s| s.as_str()));
            git(dir, &args).await
        }
        _ => git(dir, &["add", "-A"]).await,
    };
    Ok(match result {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    })
}

/// `git:commitCreate`. Runs `git commit -m <message>` then resolves the new
/// `HEAD` hash. A commit failure (nothing staged, ...) and a rev-parse
/// failure both collapse to `{ ok: false, error }` — never an `Err`.
pub async fn commit_create(dir: &str, message: &str) -> Result<Value, String> {
    match git(dir, &["commit", "-m", message]).await {
        Ok(_) => Ok(match git(dir, &["rev-parse", "HEAD"]).await {
            Ok(hash) => json!({ "ok": true, "hash": hash.trim() }),
            Err(e) => json!({ "ok": false, "error": e }),
        }),
        Err(e) => Ok(json!({ "ok": false, "error": e })),
    }
}

/// `git:push`. `git push`, collapsing success/failure into `{ ok,
/// error? }` like `checkout`. The ipc wrapper audits the egress via
/// `events::log_event` regardless of this outcome.
pub async fn push(dir: &str) -> Result<Value, String> {
    Ok(match git(dir, &["push"]).await {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": e }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("failed to spawn git for test setup");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A throwaway repo with local (not global) identity/signing config, so
    /// these tests are hermetic regardless of the machine's own git config.
    fn init_repo() -> tempfile::TempDir {
        let tmp = tempdir().unwrap();
        run(tmp.path(), &["init", "-b", "main"]);
        run(tmp.path(), &["config", "user.email", "test@example.com"]);
        run(tmp.path(), &["config", "user.name", "Test"]);
        run(tmp.path(), &["config", "commit.gpgsign", "false"]);
        tmp
    }

    fn write_and_commit(dir: &Path, file: &str, content: &str, message: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-m", message]);
    }

    fn head_hash(dir: &Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn info_reports_repo_true_with_status_counts() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "hello\n", "initial");
        std::fs::write(repo.path().join("a.txt"), "hello again\n").unwrap(); // modified
        std::fs::write(repo.path().join("b.txt"), "new\n").unwrap(); // untracked -> added

        let v = info(repo.path().to_str().unwrap()).await;
        assert_eq!(v["repo"], true);
        assert_eq!(v["branch"], "main");
        assert_eq!(v["modified"], 1);
        assert_eq!(v["added"], 1);
        assert_eq!(v["deleted"], 0);
    }

    #[tokio::test]
    async fn info_reports_repo_false_outside_a_repo() {
        let tmp = tempdir().unwrap();
        let v = info(tmp.path().to_str().unwrap()).await;
        assert_eq!(v["repo"], false);
        assert_eq!(v.as_object().unwrap().len(), 1); // no other keys leak through
    }

    #[tokio::test]
    async fn branches_lists_and_filters_blank_lines() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "initial");
        run(repo.path(), &["branch", "feature-a"]);

        let v = branches(repo.path().to_str().unwrap()).await.unwrap();
        let list: Vec<&str> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        assert!(list.contains(&"main"));
        assert!(list.contains(&"feature-a"));
        assert!(list.iter().all(|s| !s.is_empty()));
    }

    #[tokio::test]
    async fn checkout_switches_branch_and_reports_ok_true() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "initial");
        let dir = repo.path().to_str().unwrap();

        let v = checkout(dir, "new-branch", true).await;
        assert_eq!(v["ok"], true);

        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "new-branch");
    }

    #[tokio::test]
    async fn checkout_reports_ok_false_with_error_for_a_missing_branch() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "initial");

        let v = checkout(repo.path().to_str().unwrap(), "does-not-exist", false).await;
        assert_eq!(v["ok"], false);
        assert!(!v["error"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn log_returns_newest_first_with_expected_fields() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "first");
        write_and_commit(repo.path(), "a.txt", "2", "second");

        let v = log(repo.path().to_str().unwrap(), None).await.unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["subject"], "second");
        assert_eq!(arr[1]["subject"], "first");
        let hash = arr[0]["hash"].as_str().unwrap();
        assert!(!hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!arr[0]["short"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn log_zero_limit_is_falsy_and_defaults_to_250() {
        let repo = init_repo();
        for i in 0..3 {
            write_and_commit(repo.path(), "a.txt", &i.to_string(), &format!("commit {i}"));
        }
        let dir = repo.path().to_str().unwrap();

        let limited = log(dir, Some(2)).await.unwrap();
        assert_eq!(limited.as_array().unwrap().len(), 2);

        let zero = log(dir, Some(0)).await.unwrap();
        assert_eq!(zero.as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn commit_reports_body_and_changed_files_for_a_normal_commit() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "initial");
        write_and_commit(repo.path(), "a.txt", "2", "second commit\n\nwith a body");
        let hash = head_hash(repo.path());

        let v = commit(repo.path().to_str().unwrap(), &hash).await.unwrap();
        assert!(v["body"].as_str().unwrap().starts_with("second commit"));
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["status"], "M");
    }

    #[tokio::test]
    async fn commit_falls_back_to_diff_tree_for_the_root_commit() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1", "root commit");
        let hash = head_hash(repo.path());

        let v = commit(repo.path().to_str().unwrap(), &hash).await.unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[0]["status"], "A");
    }

    #[tokio::test]
    async fn diff_returns_patch_text_for_a_normal_commit() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1\n", "initial");
        write_and_commit(repo.path(), "a.txt", "2\n", "second");
        let hash = head_hash(repo.path());

        let v = diff(repo.path().to_str().unwrap(), &hash, "a.txt")
            .await
            .unwrap();
        let text = v.as_str().unwrap();
        assert!(text.contains("-1"));
        assert!(text.contains("+2"));
    }

    #[tokio::test]
    async fn diff_falls_back_to_show_for_the_root_commit() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1\n", "root commit");
        let hash = head_hash(repo.path());

        let v = diff(repo.path().to_str().unwrap(), &hash, "a.txt")
            .await
            .unwrap();
        assert!(v.as_str().unwrap().contains("a.txt"));
    }

    #[tokio::test]
    async fn git_helper_prefers_stderr_and_never_panics_on_a_bad_dir() {
        let tmp = tempdir().unwrap();
        let err = git(
            tmp.path().to_str().unwrap(),
            &["rev-parse", "--abbrev-ref", "HEAD"],
        )
        .await
        .unwrap_err();
        assert!(!err.is_empty());
    }

    #[tokio::test]
    async fn status_lists_modified_untracked_and_staged_files() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1\n", "initial");
        std::fs::write(repo.path().join("a.txt"), "2\n").unwrap(); // modified
        std::fs::write(repo.path().join("b.txt"), "new\n").unwrap(); // untracked
        run(repo.path(), &["add", "b.txt"]); // staged

        let v = status(repo.path().to_str().unwrap()).await.unwrap();
        let files = v["files"].as_array().unwrap();
        let a = files.iter().find(|f| f["path"] == "a.txt").unwrap();
        let b = files.iter().find(|f| f["path"] == "b.txt").unwrap();
        assert!(a["x"] == "M" || a["y"] == "M");
        assert_eq!(b["x"], "A");
    }

    #[tokio::test]
    async fn stage_adds_an_untracked_file() {
        let repo = init_repo();
        std::fs::write(repo.path().join("b.txt"), "new\n").unwrap(); // untracked

        let v = stage(repo.path().to_str().unwrap(), None).await.unwrap();
        assert_eq!(v["ok"], true);

        let out = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text
            .lines()
            .any(|l| l.starts_with('A') && l.contains("b.txt")));
    }

    #[tokio::test]
    async fn commit_create_makes_a_commit_and_returns_hash() {
        let repo = init_repo();
        std::fs::write(repo.path().join("a.txt"), "1\n").unwrap();
        run(repo.path(), &["add", "."]);

        let v = commit_create(repo.path().to_str().unwrap(), "hello")
            .await
            .unwrap();
        assert_eq!(v["ok"], true);
        let hash = v["hash"].as_str().unwrap();
        assert!(!hash.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn commit_create_reports_ok_false_with_no_staged_changes() {
        let repo = init_repo();
        let v = commit_create(repo.path().to_str().unwrap(), "msg")
            .await
            .unwrap();
        assert_eq!(v["ok"], false);
        assert!(!v["error"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn push_reports_ok_false_without_a_remote() {
        let repo = init_repo();
        write_and_commit(repo.path(), "a.txt", "1\n", "initial");

        let v = push(repo.path().to_str().unwrap()).await.unwrap();
        assert_eq!(v["ok"], false);
        assert!(!v["error"].as_str().unwrap().is_empty());
    }
}
