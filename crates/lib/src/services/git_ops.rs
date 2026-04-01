//! Git operation detection from command output.
//!
//! Parses bash tool output to detect git commits, PR creates,
//! branch operations, and other git events. Used for tracking
//! and telemetry.

use regex::Regex;
use std::sync::LazyLock;

/// A detected git operation.
#[derive(Debug, Clone)]
pub struct GitOperation {
    pub kind: GitOpKind,
    /// Extracted data (commit SHA, branch name, PR URL, etc.)
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitOpKind {
    Commit,
    Push,
    PrCreate,
    PrMerge,
    BranchCreate,
    BranchSwitch,
    Merge,
    Rebase,
    Stash,
    Tag,
}

static COMMIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(\S+)\s+([a-f0-9]{7,40})\]").unwrap());

static PUSH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:To\s+\S+|->)\s+(\S+)").unwrap());

static PR_CREATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https://github\.com/[^\s]+/pull/(\d+)").unwrap());

static GH_PR_CREATE_CMD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"gh\s+pr\s+create").unwrap());

static GH_PR_MERGE_CMD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"gh\s+pr\s+merge").unwrap());

static BRANCH_CREATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git\s+(?:checkout\s+-b|switch\s+-c)\s+(\S+)").unwrap());

static BRANCH_SWITCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Switched to (?:a new )?branch '(\S+)'").unwrap());

/// Detect git operations from a command string and its output.
pub fn detect_git_ops(command: &str, output: &str) -> Vec<GitOperation> {
    let mut ops = Vec::new();

    // Commit detection (from output).
    if let Some(cap) = COMMIT_RE.captures(output) {
        ops.push(GitOperation {
            kind: GitOpKind::Commit,
            detail: cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        });
    }

    // Push detection.
    if command.contains("git push")
        && let Some(cap) = PUSH_RE.captures(output)
    {
        ops.push(GitOperation {
            kind: GitOpKind::Push,
            detail: cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        });
    }

    // PR create (from command or output).
    if GH_PR_CREATE_CMD.is_match(command) {
        let url = PR_CREATE_RE
            .find(output)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        ops.push(GitOperation {
            kind: GitOpKind::PrCreate,
            detail: url,
        });
    }

    // PR merge.
    if GH_PR_MERGE_CMD.is_match(command) {
        ops.push(GitOperation {
            kind: GitOpKind::PrMerge,
            detail: String::new(),
        });
    }

    // Branch create.
    if let Some(cap) = BRANCH_CREATE_RE.captures(command) {
        ops.push(GitOperation {
            kind: GitOpKind::BranchCreate,
            detail: cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        });
    }

    // Branch switch (from output).
    if let Some(cap) = BRANCH_SWITCH_RE.captures(output) {
        ops.push(GitOperation {
            kind: GitOpKind::BranchSwitch,
            detail: cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default(),
        });
    }

    // Merge.
    if command.contains("git merge") {
        ops.push(GitOperation {
            kind: GitOpKind::Merge,
            detail: String::new(),
        });
    }

    // Rebase.
    if command.contains("git rebase") {
        ops.push(GitOperation {
            kind: GitOpKind::Rebase,
            detail: String::new(),
        });
    }

    // Stash.
    if command.contains("git stash") {
        ops.push(GitOperation {
            kind: GitOpKind::Stash,
            detail: String::new(),
        });
    }

    // Tag.
    if command.contains("git tag") {
        ops.push(GitOperation {
            kind: GitOpKind::Tag,
            detail: String::new(),
        });
    }

    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_commit() {
        let ops = detect_git_ops(
            "git commit -m 'test'",
            "[main abc1234] test commit\n 1 file changed",
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, GitOpKind::Commit);
        assert_eq!(ops[0].detail, "abc1234");
    }

    #[test]
    fn test_detect_pr_create() {
        let ops = detect_git_ops(
            "gh pr create --title 'fix'",
            "https://github.com/owner/repo/pull/42",
        );
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].kind, GitOpKind::PrCreate);
        assert!(ops[0].detail.contains("pull/42"));
    }

    #[test]
    fn test_detect_branch_switch() {
        let ops = detect_git_ops(
            "git checkout -b feature",
            "Switched to a new branch 'feature'",
        );
        assert!(ops.iter().any(|o| o.kind == GitOpKind::BranchCreate));
        assert!(ops.iter().any(|o| o.kind == GitOpKind::BranchSwitch));
    }

    #[test]
    fn test_detect_push() {
        let ops = detect_git_ops(
            "git push origin main",
            "To github.com:owner/repo.git\n   abc123..def456  main -> main",
        );
        assert!(ops.iter().any(|o| o.kind == GitOpKind::Push));
    }

    #[test]
    fn test_no_false_positives() {
        let ops = detect_git_ops("ls -la", "total 42\ndrwxr-xr-x");
        assert!(ops.is_empty());
    }
}
