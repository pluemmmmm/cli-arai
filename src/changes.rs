use similar::{ChangeTag, TextDiff};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    pub status: String,
    pub added: usize,
    pub removed: usize,
    pub diff_lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub tag: DiffTag,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffTag {
    Header,
    Added,
    Removed,
    Context,
}

#[derive(Debug, Clone)]
pub struct CommitGraphRow {
    pub graph: String,
    pub hash: String,
    pub refs: Vec<String>,
    pub subject: String,
    pub author: String,
    pub date: String,
}

/// Collect real git commit graph rows via `git log --graph --all`.
pub fn collect_commit_graph(cwd: &Path, limit: usize) -> Vec<CommitGraphRow> {
    let sep = "\x1f";
    let fmt = format!("%h{sep}%D{sep}%s{sep}%an{sep}%ar");
    let output = Command::new("git")
        .args([
            "log",
            "--graph",
            "--all",
            &format!("-n{limit}"),
            &format!("--pretty=format:\x1e{fmt}"),
        ])
        .current_dir(cwd)
        .output();

    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut rows = Vec::new();
    for line in text.lines() {
        if let Some((graph_part, rest)) = line.split_once('\x1e') {
            let parts: Vec<&str> = rest.split('\x1f').collect();
            let hash = parts.first().copied().unwrap_or("").trim().to_string();
            let refs_raw = parts.get(1).copied().unwrap_or("").trim();
            let refs: Vec<String> = if refs_raw.is_empty() {
                Vec::new()
            } else {
                refs_raw
                    .split(',')
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .collect()
            };
            let subject = parts.get(2).copied().unwrap_or("").trim().to_string();
            let author = parts.get(3).copied().unwrap_or("").trim().to_string();
            let date = parts.get(4).copied().unwrap_or("").trim().to_string();

            rows.push(CommitGraphRow {
                graph: graph_part.to_string(),
                hash,
                refs,
                subject,
                author,
                date,
            });
        } else if !line.trim().is_empty() {
            // Pure connector line (e.g. `|\` or `|/`)
            rows.push(CommitGraphRow {
                graph: line.to_string(),
                hash: String::new(),
                refs: Vec::new(),
                subject: String::new(),
                author: String::new(),
                date: String::new(),
            });
        }
    }

    rows
}

/// Collect the real file diffs introduced by a specific commit hash (`git show`).
pub fn collect_commit_diff(cwd: &Path, hash: &str) -> Vec<FileChange> {
    if hash.is_empty() {
        return Vec::new();
    }
    let output = Command::new("git")
        .args(["show", "--pretty=format:", "--patch", hash])
        .current_dir(cwd)
        .output();

    let text = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut changes: Vec<FileChange> = Vec::new();
    let mut cur_path = String::new();
    let mut cur_status = "M".to_string();
    let mut cur_added = 0usize;
    let mut cur_removed = 0usize;
    let mut cur_lines: Vec<DiffLine> = Vec::new();
    let mut old_ln = 1usize;
    let mut new_ln = 1usize;

    let flush = |changes: &mut Vec<FileChange>,
                 path: &mut String,
                 status: &mut String,
                 added: &mut usize,
                 removed: &mut usize,
                 lines: &mut Vec<DiffLine>| {
        if !path.is_empty() {
            changes.push(FileChange {
                path: std::mem::take(path),
                status: std::mem::replace(status, "M".to_string()),
                added: std::mem::take(added),
                removed: std::mem::take(removed),
                diff_lines: std::mem::take(lines),
            });
        }
    };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(
                &mut changes,
                &mut cur_path,
                &mut cur_status,
                &mut cur_added,
                &mut cur_removed,
                &mut cur_lines,
            );
            cur_path = rest
                .split(" b/")
                .nth(1)
                .unwrap_or(rest)
                .trim()
                .to_string();
            cur_status = "M".to_string();
        } else if line.starts_with("new file mode") {
            cur_status = "NEW".to_string();
        } else if line.starts_with("deleted file mode") {
            cur_status = "D".to_string();
        } else if line.starts_with("@@") {
            let (o, n) = parse_hunk_start(line);
            old_ln = o;
            new_ln = n;
            cur_lines.push(DiffLine {
                tag: DiffTag::Header,
                old_line: None,
                new_line: None,
                content: line.to_string(),
            });
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("index ")
            || line.starts_with("old mode")
            || line.starts_with("new mode")
        {
            continue;
        } else if !cur_path.is_empty() {
            if line.starts_with('+') {
                cur_added += 1;
                cur_lines.push(DiffLine {
                    tag: DiffTag::Added,
                    old_line: None,
                    new_line: Some(new_ln),
                    content: line.to_string(),
                });
                new_ln += 1;
            } else if line.starts_with('-') {
                cur_removed += 1;
                cur_lines.push(DiffLine {
                    tag: DiffTag::Removed,
                    old_line: Some(old_ln),
                    new_line: None,
                    content: line.to_string(),
                });
                old_ln += 1;
            } else if line.starts_with(' ') {
                cur_lines.push(DiffLine {
                    tag: DiffTag::Context,
                    old_line: Some(old_ln),
                    new_line: Some(new_ln),
                    content: line.to_string(),
                });
                old_ln += 1;
                new_ln += 1;
            }
        }
    }

    flush(
        &mut changes,
        &mut cur_path,
        &mut cur_status,
        &mut cur_added,
        &mut cur_removed,
        &mut cur_lines,
    );

    changes
}

#[derive(Debug, Default, Clone)]
pub struct ChangeTracker {
    recorded: Arc<Mutex<Vec<FileChange>>>,
}

impl ChangeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a file modification made by the CLI or a Rune script.
    pub fn record_edit(&self, path: &str, old_content: Option<&str>, new_content: &str) {
        let old = old_content.unwrap_or("");
        let status = if old_content.is_none() { "A" } else { "M" }.to_string();
        let diff = TextDiff::from_lines(old, new_content);

        let mut added = 0;
        let mut removed = 0;
        let mut old_ln = 1usize;
        let mut new_ln = 1usize;
        let mut diff_lines = vec![DiffLine {
            tag: DiffTag::Header,
            old_line: None,
            new_line: None,
            content: format!("@@ {path} @@"),
        }];

        for change in diff.iter_all_changes() {
            let text = change.value().trim_end_matches('\n').to_string();
            match change.tag() {
                ChangeTag::Delete => {
                    removed += 1;
                    diff_lines.push(DiffLine {
                        tag: DiffTag::Removed,
                        old_line: Some(old_ln),
                        new_line: None,
                        content: format!("-{text}"),
                    });
                    old_ln += 1;
                }
                ChangeTag::Insert => {
                    added += 1;
                    diff_lines.push(DiffLine {
                        tag: DiffTag::Added,
                        old_line: None,
                        new_line: Some(new_ln),
                        content: format!("+{text}"),
                    });
                    new_ln += 1;
                }
                ChangeTag::Equal => {
                    diff_lines.push(DiffLine {
                        tag: DiffTag::Context,
                        old_line: Some(old_ln),
                        new_line: Some(new_ln),
                        content: format!(" {text}"),
                    });
                    old_ln += 1;
                    new_ln += 1;
                }
            }
        }

        if let Ok(mut guard) = self.recorded.lock() {
            guard.retain(|c| c.path != path);
            guard.push(FileChange {
                path: path.to_string(),
                status,
                added,
                removed,
                diff_lines,
            });
        }
    }

    /// Collect both git working tree changes and any in-session recorded edits.
    pub fn snapshot(&self, cwd: &Path) -> Vec<FileChange> {
        let mut changes = collect_git_changes(cwd);

        if let Ok(guard) = self.recorded.lock() {
            for rec in guard.iter() {
                if !changes.iter().any(|c| c.path == rec.path) {
                    changes.push(rec.clone());
                }
            }
        }

        changes
    }
}

fn collect_git_changes(cwd: &Path) -> Vec<FileChange> {
    let status_out = match Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(cwd)
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };

    let mut results = Vec::new();
    for line in status_out.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].trim().to_string();
        let path = line[3..].trim().trim_matches('"').to_string();

        // Skip Rust build artifacts even in projects without .gitignore
        if path.starts_with("target/") || path == "target" {
            continue;
        }

        let (added, removed, diff_lines) = if status == "??" {
            let full_path = cwd.join(&path);
            if full_path.is_dir() {
                continue;
            }
            let content = std::fs::read_to_string(&full_path).unwrap_or_default();
            let mut lines = vec![DiffLine {
                tag: DiffTag::Header,
                old_line: None,
                new_line: None,
                content: format!("@@ new file: {path} @@"),
            }];
            let mut count = 0;
            for (idx, l) in content.lines().take(400).enumerate() {
                count += 1;
                lines.push(DiffLine {
                    tag: DiffTag::Added,
                    old_line: None,
                    new_line: Some(idx + 1),
                    content: format!("+{l}"),
                });
            }
            (count, 0, lines)
        } else {
            git_diff_for_file(cwd, &path)
        };

        let display_status = if status == "??" {
            "NEW".to_string()
        } else if status.is_empty() {
            "M".to_string()
        } else {
            status
        };

        results.push(FileChange {
            path,
            status: display_status,
            added,
            removed,
            diff_lines,
        });
    }

    results
}

fn parse_hunk_start(hunk_header: &str) -> (usize, usize) {
    // Example: @@ -12,7 +12,9 @@ fn main()
    let mut old_start = 1usize;
    let mut new_start = 1usize;
    for part in hunk_header.split_whitespace() {
        if let Some(rest) = part.strip_prefix('-') {
            if let Some(num) = rest.split(',').next().and_then(|s| s.parse::<usize>().ok()) {
                old_start = num;
            }
        } else if let Some(rest) = part.strip_prefix('+') {
            if let Some(num) = rest.split(',').next().and_then(|s| s.parse::<usize>().ok()) {
                new_start = num;
            }
        }
    }
    (old_start, new_start)
}

fn git_diff_for_file(cwd: &Path, path: &str) -> (usize, usize, Vec<DiffLine>) {
    // Prefer `git diff HEAD` so both staged & unstaged changes match VS Code / Orca.
    // Fall back to `git diff` / `git diff --cached` if the repo has no commits yet.
    let head_out = Command::new("git")
        .args(["diff", "HEAD", "--", path])
        .current_dir(cwd)
        .output();

    let text = match head_out {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            String::from_utf8_lossy(&o.stdout).to_string()
        }
        _ => {
            let unstaged = Command::new("git")
                .args(["diff", "--", path])
                .current_dir(cwd)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();

            if !unstaged.is_empty() {
                unstaged
            } else {
                Command::new("git")
                    .args(["diff", "--cached", "--", path])
                    .current_dir(cwd)
                    .output()
                    .ok()
                    .filter(|o| o.status.success())
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default()
            }
        }
    };

    if text.is_empty() {
        return (0, 0, Vec::new());
    }

    let mut added = 0;
    let mut removed = 0;
    let mut old_ln = 1usize;
    let mut new_ln = 1usize;
    let mut lines = Vec::new();

    for line in text.lines() {
        if line.starts_with("@@") {
            let (o, n) = parse_hunk_start(line);
            old_ln = o;
            new_ln = n;
            lines.push(DiffLine {
                tag: DiffTag::Header,
                old_line: None,
                new_line: None,
                content: line.to_string(),
            });
        } else if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("diff ")
            || line.starts_with("index ")
        {
            continue;
        } else if line.starts_with('+') {
            added += 1;
            lines.push(DiffLine {
                tag: DiffTag::Added,
                old_line: None,
                new_line: Some(new_ln),
                content: line.to_string(),
            });
            new_ln += 1;
        } else if line.starts_with('-') {
            removed += 1;
            lines.push(DiffLine {
                tag: DiffTag::Removed,
                old_line: Some(old_ln),
                new_line: None,
                content: line.to_string(),
            });
            old_ln += 1;
        } else {
            lines.push(DiffLine {
                tag: DiffTag::Context,
                old_line: Some(old_ln),
                new_line: Some(new_ln),
                content: line.to_string(),
            });
            old_ln += 1;
            new_ln += 1;
        }
    }

    (added, removed, lines)
}
