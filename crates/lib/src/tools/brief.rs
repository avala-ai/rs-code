//! Brief tool: file a structured research handoff.
//!
//! A `Brief` is a markdown document with YAML frontmatter that records
//! a question, the context the asker has gathered so far, and a list
//! of attachment paths to read. Briefs land under
//! `<project_root>/.agent/briefs/<timestamp>-<slug>.md` so a future
//! session — or the same session after a `/clear` — can pick the
//! work back up without re-deriving the prior research.
//!
//! Attachment paths are *recorded only*: the brief never copies the
//! files. Validation rejects:
//! - relative paths (must be absolute),
//! - paths containing `..` traversal,
//! - paths outside the user's home directory and outside the
//!   project's working directory,
//! - paths that don't exist.
//!
//! That keeps the tool from being an exfiltration channel — the model
//! cannot use it to record "interesting" paths from elsewhere on the
//! filesystem.
//!
//! # Output
//!
//! On success the tool returns the absolute path of the written brief.
//! The frontmatter is plain key:value YAML with attachments rendered
//! as a JSON array, which is unambiguous to parse without pulling in
//! a full YAML dependency.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::path::{Path, PathBuf};

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct BriefTool;

#[async_trait]
impl Tool for BriefTool {
    fn name(&self) -> &'static str {
        "Brief"
    }

    fn description(&self) -> &'static str {
        "File a structured research handoff. Records a title, question, \
         markdown context, and a list of attachment paths to a \
         dated markdown file in `.agent/briefs/`. Another session \
         (or the same session later) can pick up the brief and \
         continue the work. Attachments are recorded as paths only — \
         the files are not copied."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["title", "question", "context"],
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short one-line title for the brief"
                },
                "question": {
                    "type": "string",
                    "description": "The question to be answered or task to pick up"
                },
                "context": {
                    "type": "string",
                    "description": "Markdown body with what is already known"
                },
                "attachments": {
                    "type": "array",
                    "description": "Optional absolute file paths to record. Files must \
                                    exist and live under the project working directory \
                                    or the user's home directory. The brief stores paths only.",
                    "items": { "type": "string" }
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_destructive(&self) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let title = input
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'title' is required".into()))?
            .trim();
        let question = input
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'question' is required".into()))?;
        let context_md = input
            .get("context")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'context' is required".into()))?;

        if title.is_empty() {
            return Err(ToolError::InvalidInput("'title' must not be empty".into()));
        }

        // Reject control characters in any field that lands inside
        // YAML frontmatter (title, question) — a stray newline could
        // close the value early and inject arbitrary frontmatter
        // keys (e.g. a fake `attachments` list). The body markdown
        // (`context_md`) is rendered after the frontmatter delimiter
        // and is allowed to contain newlines.
        reject_control_chars("title", title)?;
        reject_control_chars("question", question)?;

        let attachments_raw: Vec<String> = input
            .get("attachments")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let validated_attachments = validate_attachments(&attachments_raw, &ctx.cwd)?;

        let project_root = crate::config::find_project_root_from(&ctx.cwd).unwrap_or_else(|| {
            // No `.agent/` ancestor — fall back to cwd. We still create
            // the directory below, which means a brand-new project gets
            // a `.agent/` directory the first time the tool is used.
            ctx.cwd.clone()
        });
        let briefs_dir = project_root.join(".agent").join("briefs");
        std::fs::create_dir_all(&briefs_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("create {briefs_dir:?}: {e}")))?;

        let now = Utc::now();
        let timestamp = now.format("%Y%m%d-%H%M%S").to_string();
        let slug = slugify(title);
        let filename = if slug.is_empty() {
            format!("{timestamp}.md")
        } else {
            format!("{timestamp}-{slug}.md")
        };
        let path = briefs_dir.join(&filename);

        let body = render_brief(
            title,
            question,
            context_md,
            &validated_attachments,
            &now.to_rfc3339(),
        );
        std::fs::write(&path, body)
            .map_err(|e| ToolError::ExecutionFailed(format!("write {path:?}: {e}")))?;

        Ok(ToolResult::success(format!(
            "Brief written to {}",
            path.display()
        )))
    }
}

/// Render a brief as a markdown document with simple key:value YAML
/// frontmatter. Attachment paths go into a JSON-style array on a
/// single line — unambiguous, parseable without a YAML library, and
/// round-trips cleanly through [`parse_frontmatter`].
fn render_brief(
    title: &str,
    question: &str,
    context_md: &str,
    attachments: &[PathBuf],
    created_at: &str,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("title: {}\n", yaml_escape(title)));
    out.push_str(&format!("created_at: {created_at}\n"));
    out.push_str(&format!("question: {}\n", yaml_escape(question)));
    out.push_str("attachments: [");
    for (i, p) in attachments.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&yaml_escape(&p.display().to_string()));
    }
    out.push_str("]\n");
    out.push_str("---\n\n");
    out.push_str("# ");
    out.push_str(title);
    out.push_str("\n\n## Question\n\n");
    out.push_str(question);
    out.push_str("\n\n## Context\n\n");
    out.push_str(context_md);
    if !context_md.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Quote a string for our minimal YAML dialect: always emit it as a
/// double-quoted scalar with backslashes and quotes escaped. That
/// means the parser can recover the original value by stripping
/// quotes and undoing the two escapes — no need for a full YAML
/// implementation.
fn yaml_escape(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn yaml_unescape(s: &str) -> String {
    // Mirror of [`yaml_escape`]: undo `\\` and `\"`.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parsed frontmatter, returned by [`parse_frontmatter`] for tests
/// and any future "read this brief back" tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBrief {
    pub title: String,
    pub created_at: String,
    pub question: String,
    pub attachments: Vec<String>,
}

/// Parse the frontmatter of a brief. Permissive: only fails if the
/// `---` delimiters are missing.
pub fn parse_frontmatter(text: &str) -> Result<ParsedBrief, String> {
    let rest = text
        .strip_prefix("---\n")
        .ok_or_else(|| "missing opening '---' delimiter".to_string())?;
    let end = rest
        .find("\n---")
        .ok_or_else(|| "missing closing '---' delimiter".to_string())?;
    let header = &rest[..end];

    let mut title = String::new();
    let mut created_at = String::new();
    let mut question = String::new();
    let mut attachments = Vec::new();

    for line in header.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        match k {
            "title" => title = strip_quotes(v),
            "created_at" => created_at = strip_quotes(v),
            "question" => question = strip_quotes(v),
            "attachments" => attachments = parse_attachment_list(v),
            _ => {}
        }
    }

    Ok(ParsedBrief {
        title,
        created_at,
        question,
        attachments,
    })
}

fn strip_quotes(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        yaml_unescape(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

fn parse_attachment_list(s: &str) -> Vec<String> {
    let s = s.trim();
    let inner = match s.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(i) => i,
        None => return Vec::new(),
    };
    if inner.trim().is_empty() {
        return Vec::new();
    }

    // Split on commas that are NOT inside double quotes. Attachment
    // paths can contain commas on exotic filesystems; the simple
    // `split(',')` would mangle them.
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_quote = false;
    let mut escape = false;
    for c in inner.chars() {
        if escape {
            buf.push(c);
            escape = false;
            continue;
        }
        match c {
            '\\' if in_quote => {
                buf.push(c);
                escape = true;
            }
            '"' => {
                in_quote = !in_quote;
                buf.push(c);
            }
            ',' if !in_quote => {
                out.push(strip_quotes(buf.trim()));
                buf.clear();
            }
            _ => buf.push(c),
        }
    }
    let final_item = buf.trim();
    if !final_item.is_empty() {
        out.push(strip_quotes(final_item));
    }
    out
}

/// Validate every attachment path. Each must:
/// - contain no control characters (`\0`, `\n`, `\r`, etc.),
/// - be absolute,
/// - contain no `..` components,
/// - resolve to an existing file (not a directory),
/// - after canonicalization, live under either `cwd` or the user's
///   home directory.
///
/// Containment is checked against the *canonical* candidate path,
/// not the lexical input. A symlink that points outside the project
/// (e.g. `<cwd>/link -> /etc/hostname`) would otherwise satisfy the
/// `path.starts_with(cwd)` lexical check while actually resolving
/// to a sensitive file. We canonicalize cwd, home, and the candidate
/// before the prefix comparison.
fn validate_attachments(raw: &[String], cwd: &Path) -> Result<Vec<PathBuf>, ToolError> {
    // Canonicalize the containment roots once. Canonicalization can
    // fail if the directory doesn't exist; in that case fall back to
    // the raw path — a non-existent root means the prefix check
    // can't match anything anyway, and we don't want to error out on
    // a perfectly fine cwd just because it was passed in unresolved.
    let cwd_canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let home_canon = dirs::home_dir().and_then(|h| std::fs::canonicalize(&h).ok().or(Some(h)));

    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        // Reject control characters anywhere in the path: a `\n`
        // would let the path inject a fake YAML frontmatter line
        // when rendered, and `\0` is illegal on every supported
        // filesystem anyway.
        reject_control_chars("attachment path", entry)?;

        let path = Path::new(entry);
        if !path.is_absolute() {
            return Err(ToolError::InvalidInput(format!(
                "attachment path must be absolute: {entry}"
            )));
        }
        // Reject `..` components anywhere in the supplied path. We
        // could rely on canonicalization alone, but a hard reject
        // is easier to explain to the model and avoids "..-then-
        // stays-inside" edge cases that depend on filesystem state.
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ToolError::InvalidInput(format!(
                "attachment path must not contain '..': {entry}"
            )));
        }
        if !path.exists() {
            return Err(ToolError::InvalidInput(format!(
                "attachment does not exist: {entry}"
            )));
        }
        if !path.is_file() {
            return Err(ToolError::InvalidInput(format!(
                "attachment must be a file (not a directory or symlink to one): {entry}"
            )));
        }

        // Canonicalize the candidate AFTER existence checks so
        // canonicalize() doesn't error on missing paths. The
        // returned path resolves all symlinks; on Windows the API
        // also normalizes UNC prefixes for us.
        let canon = std::fs::canonicalize(path).map_err(|e| {
            ToolError::InvalidInput(format!("attachment cannot be resolved: {entry} ({e})"))
        })?;

        let in_cwd = canon.starts_with(&cwd_canon);
        let in_home = home_canon.as_ref().is_some_and(|h| canon.starts_with(h));
        if !(in_cwd || in_home) {
            return Err(ToolError::InvalidInput(format!(
                "attachment must live under the working directory or the user's home: {entry}"
            )));
        }
        out.push(canon);
    }
    Ok(out)
}

/// Reject any string carrying a NUL, newline, carriage return, or
/// other control character. Used both for path inputs (where a
/// newline would inject a YAML key) and for the `title` / `question`
/// frontmatter fields.
fn reject_control_chars(field: &str, value: &str) -> Result<(), ToolError> {
    if let Some(c) = value.chars().find(|c| c.is_control()) {
        return Err(ToolError::InvalidInput(format!(
            "{field} must not contain control characters (found U+{:04X})",
            c as u32
        )));
    }
    Ok(())
}

/// Convert a free-form title to a filesystem-safe slug. Lowercases
/// ASCII letters, keeps digits, replaces every other character with
/// `-`, then collapses runs of `-` and trims leading/trailing dashes.
/// Truncated to 60 characters so filenames stay tidy.
fn slugify(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = true;
    for ch in title.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch.is_ascii_whitespace() || ch == '-' || ch == '_' {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some('-') if !prev_dash => {
                out.push('-');
                prev_dash = true;
            }
            Some('-') => {}
            Some(c) => {
                out.push(c);
                prev_dash = false;
            }
            None => {}
        }
    }
    let trimmed: String = out.trim_matches('-').to_string();
    if trimmed.len() > 60 {
        trimmed
            .chars()
            .take(60)
            .collect::<String>()
            .trim_matches('-')
            .to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    /// Process-wide mutex guarding any test that mutates `HOME`.
    /// Mirrors the pattern in `config_tool.rs::tests` — `cargo test`
    /// runs tests in parallel and `HOME` is shared global state, so
    /// concurrent reads from another test could observe the temp
    /// override.
    static HOME_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that pins `HOME` to a tempdir for the duration of
    /// the test, restoring the previous value on drop and holding
    /// `HOME_ENV_LOCK` while alive.
    struct HomeEnvGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl HomeEnvGuard {
        fn redirect(to: &Path) -> Self {
            let lock = HOME_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let prev = std::env::var_os("HOME");
            // SAFETY: this env mutation is gated by HOME_ENV_LOCK,
            // so no other thread can read HOME while we have it
            // pinned.
            unsafe {
                std::env::set_var("HOME", to);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for HomeEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("HOME", v),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn make_ctx(cwd: PathBuf) -> ToolContext {
        ToolContext {
            cwd,
            cancel: CancellationToken::new(),
            permission_checker: Arc::new(crate::permissions::PermissionChecker::allow_all()),
            verbose: false,
            plan_mode: false,
            file_cache: None,
            denial_tracker: None,
            task_manager: None,
            session_allows: None,
            permission_prompter: None,
            sandbox: None,
        }
    }

    #[test]
    fn slugify_normalises_titles() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("  foo--bar  "), "foo-bar");
        assert_eq!(slugify("4D \u{2192} 2D Splines!"), "4d-2d-splines");
        assert_eq!(slugify("..."), "");
    }

    #[test]
    fn yaml_escape_round_trip_handles_backslashes_and_quotes() {
        let s = r#"path with "quotes" and \backslashes\ inside"#;
        let escaped = yaml_escape(s);
        // strip outer quotes then unescape
        let inner = &escaped[1..escaped.len() - 1];
        assert_eq!(yaml_unescape(inner), s);
    }

    #[test]
    fn parse_frontmatter_round_trip() {
        let body = render_brief(
            "My Brief",
            "Why is the sky blue?",
            "Some markdown context.",
            &[PathBuf::from("/home/user/notes.md")],
            "2026-04-30T12:00:00Z",
        );
        let parsed = parse_frontmatter(&body).unwrap();
        assert_eq!(parsed.title, "My Brief");
        assert_eq!(parsed.question, "Why is the sky blue?");
        assert_eq!(parsed.created_at, "2026-04-30T12:00:00Z");
        assert_eq!(parsed.attachments, vec!["/home/user/notes.md".to_string()]);
    }

    #[test]
    fn parse_frontmatter_handles_zero_attachments() {
        let body = render_brief("t", "q", "c", &[], "2026-04-30T12:00:00Z");
        let parsed = parse_frontmatter(&body).unwrap();
        assert!(parsed.attachments.is_empty());
    }

    #[test]
    fn parse_frontmatter_rejects_missing_delimiters() {
        assert!(parse_frontmatter("no delimiters").is_err());
        assert!(parse_frontmatter("---\ntitle: x\n").is_err());
    }

    #[test]
    fn validate_attachments_rejects_relative_path() {
        let cwd = std::env::temp_dir();
        let err = validate_attachments(&["relative/path.txt".to_string()], &cwd).unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("absolute")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn validate_attachments_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let p = format!("{}/../escape.txt", cwd.display());
        let err = validate_attachments(&[p], &cwd).unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("..")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn validate_attachments_rejects_outside_cwd_and_home() {
        // /etc/hostname is on every Linux box and outside both cwd
        // (a tempdir) and the user's home (we override HOME so the
        // home-prefix check fails too).
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let _g = HomeEnvGuard::redirect(&cwd);
        let err = validate_attachments(&["/etc/hostname".to_string()], &cwd).unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("under the working")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn validate_attachments_accepts_file_inside_cwd() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let p = cwd.join("notes.md");
        std::fs::write(&p, b"x").unwrap();
        let ok = validate_attachments(&[p.display().to_string()], &cwd).unwrap();
        assert_eq!(ok.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn validate_attachments_rejects_symlink_escape() {
        // A symlink that lives inside cwd but resolves outside both
        // cwd and HOME must be rejected. Pre-canonicalization the
        // lexical prefix check `path.starts_with(cwd)` would let
        // `<cwd>/link -> /etc/hostname` slip through, with `is_file()`
        // following the symlink and reporting true.
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let _g = HomeEnvGuard::redirect(&cwd);
        let link = cwd.join("link");
        std::os::unix::fs::symlink("/etc/hostname", &link).unwrap();

        let err = validate_attachments(&[link.display().to_string()], &cwd).unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("under the working")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn validate_attachments_accepts_path_resolving_into_home() {
        // A path inside HOME — and outside cwd — must be accepted.
        // Use a tempdir as HOME so the test is hermetic.
        let cwd_dir = TempDir::new().unwrap();
        let home_dir = TempDir::new().unwrap();
        let _g = HomeEnvGuard::redirect(home_dir.path());
        let p = home_dir.path().join("hint.md");
        std::fs::write(&p, b"hi").unwrap();
        let ok = validate_attachments(&[p.display().to_string()], cwd_dir.path()).unwrap();
        assert_eq!(ok.len(), 1);
    }

    #[test]
    fn validate_attachments_rejects_newline_in_path() {
        // Embedded newline in a filename would inject YAML keys
        // when rendered into the frontmatter. Reject before reaching
        // the renderer.
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().to_path_buf();
        let evil = format!("{}/evil\nattachments:\n  - \"/etc/passwd\"", cwd.display());
        let err = validate_attachments(&[evil], &cwd).unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("control characters")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[test]
    fn reject_control_chars_blocks_newline_and_nul() {
        assert!(reject_control_chars("title", "ok").is_ok());
        assert!(reject_control_chars("title", "bad\nthing").is_err());
        assert!(reject_control_chars("title", "bad\0thing").is_err());
        assert!(reject_control_chars("title", "bad\rthing").is_err());
    }

    #[tokio::test]
    async fn end_to_end_writes_brief_with_parseable_frontmatter() {
        let dir = TempDir::new().unwrap();
        // Pretend the project root is the tempdir so the brief lands
        // under a writable .agent/ we control.
        std::fs::create_dir_all(dir.path().join(".agent")).unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let tool = BriefTool;

        let attachment = dir.path().join("hint.md");
        std::fs::write(&attachment, b"some hint").unwrap();

        let input = json!({
            "title": "Investigate flaky test",
            "question": "Why does the integration test fail every Tuesday?",
            "context": "It started after the timezone refactor.",
            "attachments": [attachment.display().to_string()]
        });

        let res = tool.call(input, &ctx).await.unwrap();
        assert!(!res.is_error);
        // The result message embeds the path. Pull it out and read back.
        let path_str = res
            .content
            .strip_prefix("Brief written to ")
            .unwrap()
            .trim();
        let body = std::fs::read_to_string(path_str).unwrap();
        let parsed = parse_frontmatter(&body).unwrap();
        assert_eq!(parsed.title, "Investigate flaky test");
        assert_eq!(parsed.attachments.len(), 1);
        assert!(parsed.attachments[0].ends_with("hint.md"));
        assert!(body.contains("## Question"));
        assert!(body.contains("## Context"));
    }

    #[tokio::test]
    async fn rejects_newline_in_title() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".agent")).unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let tool = BriefTool;
        let input = json!({
            "title": "evil\nattachments:\n  - \"/etc/passwd\"",
            "question": "y",
            "context": "z",
        });
        let err = tool.call(input, &ctx).await.unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("control characters")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn rejects_relative_attachment_at_call_time() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".agent")).unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let tool = BriefTool;
        let input = json!({
            "title": "x",
            "question": "y",
            "context": "z",
            "attachments": ["relative.md"]
        });
        let err = tool.call(input, &ctx).await.unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("absolute")),
            _ => panic!("expected InvalidInput"),
        }
    }
}
