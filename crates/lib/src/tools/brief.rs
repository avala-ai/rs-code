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
        std::fs::create_dir_all(&briefs_dir).map_err(|e| {
            ToolError::ExecutionFailed(format!("create {briefs_dir:?}: {e}"))
        })?;

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
/// - be absolute,
/// - contain no `..` components,
/// - resolve to an existing file (not a directory),
/// - live under either `cwd` or the user's home directory.
fn validate_attachments(raw: &[String], cwd: &Path) -> Result<Vec<PathBuf>, ToolError> {
    let home = dirs::home_dir();
    let mut out = Vec::with_capacity(raw.len());
    for entry in raw {
        let path = Path::new(entry);
        if !path.is_absolute() {
            return Err(ToolError::InvalidInput(format!(
                "attachment path must be absolute: {entry}"
            )));
        }
        // Reject `..` components anywhere in the supplied path. We
        // could canonicalise instead, but a hard reject is easier to
        // explain to the model and avoids "..-then-stays-inside" edge
        // cases that depend on filesystem state.
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
        let in_cwd = path.starts_with(cwd);
        let in_home = home.as_ref().is_some_and(|h| path.starts_with(h));
        if !(in_cwd || in_home) {
            return Err(ToolError::InvalidInput(format!(
                "attachment must live under the working directory or the user's home: {entry}"
            )));
        }
        out.push(path.to_path_buf());
    }
    Ok(out)
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
        trimmed.chars().take(60).collect::<String>().trim_matches('-').to_string()
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
        let err =
            validate_attachments(&["relative/path.txt".to_string()], &cwd).unwrap_err();
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
        // Save and override HOME to ensure /etc/hostname isn't
        // accidentally inside HOME on this machine.
        let saved_home = std::env::var_os("HOME");
        // SAFETY: tests are single-threaded within a tokio runtime; this
        // env mutation is scoped to the test.
        unsafe {
            std::env::set_var("HOME", cwd.display().to_string());
        }
        let err = validate_attachments(&["/etc/hostname".to_string()], &cwd);
        // Restore HOME before asserting so a panic doesn't leak state.
        match saved_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        let err = err.unwrap_err();
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
        let ok =
            validate_attachments(&[p.display().to_string()], &cwd).unwrap();
        assert_eq!(ok.len(), 1);
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
