//! CronCreate tool: create a stored routine that runs on a cron schedule.
//!
//! Validates the cron expression with [`crate::schedule::CronExpr`], persists
//! a [`Schedule`] via [`ScheduleStore`], and returns the routine id and the
//! next-run timestamp.

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;
use crate::permissions::{PermissionChecker, PermissionDecision};
use crate::schedule::{CronExpr, Schedule};

use super::cron_support::open_store;

pub struct CronCreateTool;

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &'static str {
        "CronCreate"
    }

    fn description(&self) -> &'static str {
        "Create a stored routine that runs a prompt on a cron schedule. \
         Returns the routine id and the next-run timestamp."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["cron_expression", "prompt"],
            "properties": {
                "cron_expression": {
                    "type": "string",
                    "description": "5-field cron expression: 'minute hour day-of-month month day-of-week'."
                },
                "prompt": {
                    "type": "string",
                    "description": "Prompt to send to the agent on each scheduled run."
                },
                "working_directory": {
                    "type": "string",
                    "description": "Optional absolute path used as the cwd for the run. Defaults to the current working directory."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional per-run timeout in seconds. Currently advisory; turn cap applies regardless."
                },
                "name": {
                    "type": "string",
                    "description": "Optional human-readable id for the routine. ASCII-printable, no path separators ('/', '\\'), no '..', no whitespace or control characters, max 64 characters. A random id is generated when omitted."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_destructive(&self) -> bool {
        // Adding a routine is a write that will spawn future agent runs
        // unattended, so treat it as a state-mutating operation that
        // should require explicit approval by default.
        true
    }

    async fn check_permissions(
        &self,
        input: &serde_json::Value,
        checker: &PermissionChecker,
    ) -> PermissionDecision {
        // Writes always go through the permission checker so users can
        // add explicit allow/deny rules for CronCreate in their config.
        checker.check(self.name(), input)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let cron_expression = input
            .get("cron_expression")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'cron_expression' is required".into()))?;

        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'prompt' is required".into()))?;

        let working_directory = input
            .get("working_directory")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let timeout_seconds = input.get("timeout_seconds").and_then(|v| v.as_u64());

        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Validate cron expression up-front using the existing parser.
        let cron = CronExpr::parse(cron_expression)
            .map_err(|e| ToolError::InvalidInput(format!("Invalid cron expression: {e}")))?;

        let cwd = working_directory.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        let id = match name {
            Some(n) => {
                validate_routine_name(&n).map_err(ToolError::InvalidInput)?;
                n
            }
            None => generate_routine_id(),
        };

        let store = open_store().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to open schedule store: {e}"))
        })?;

        // Refuse to clobber an existing routine — caller must delete first.
        if store.load(&id).is_ok() {
            return Err(ToolError::InvalidInput(format!(
                "Routine '{id}' already exists. Choose a different name or delete the existing routine first."
            )));
        }

        // Map the optional timeout into max_turns as a coarse cap. The
        // ScheduleExecutor doesn't currently honor a wall-clock timeout
        // independent of turns; advertise the field so callers can set
        // an explicit ceiling but document the limitation.
        let max_turns = timeout_seconds.map(|s| ((s / 30).max(1)) as usize);

        let schedule = Schedule {
            name: id.clone(),
            cron: cron_expression.to_string(),
            prompt: prompt.to_string(),
            cwd,
            enabled: true,
            model: None,
            permission_mode: None,
            max_cost_usd: None,
            max_turns,
            created_at: Utc::now(),
            last_run_at: None,
            last_result: None,
            webhook_secret: None,
        };

        store
            .save(&schedule)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to save routine: {e}")))?;

        let now = Utc::now().naive_utc();
        let next_run = cron
            .next_after(&now)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());

        let response = json!({
            "id": id,
            "cron_expression": cron_expression,
            "next_run_at": next_run,
            "enabled": true,
        });

        Ok(ToolResult::success(
            serde_json::to_string_pretty(&response)
                .unwrap_or_else(|_| format!("Created routine '{id}'")),
        ))
    }
}

/// Generate a short, opaque routine id when the caller doesn't supply one.
fn generate_routine_id() -> String {
    let raw = uuid::Uuid::new_v4().to_string().replace('-', "");
    format!("cron-{}", &raw[..8])
}

/// Maximum permitted length of a caller-supplied routine name.
const MAX_ROUTINE_NAME_LEN: usize = 64;

/// Validate a caller-supplied routine name before it is used as a
/// filesystem path component by [`crate::schedule::ScheduleStore`].
///
/// The schedule store derives the on-disk path as
/// `<schedules_dir>/<name>.json`, so any input that contains path
/// separators, parent-directory references, NUL bytes, or other
/// control characters could escape the schedules directory or
/// produce surprising filenames. Reject those inputs up-front with a
/// specific error so the model can self-correct.
fn validate_routine_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Routine name must not be empty.".into());
    }
    if name.len() > MAX_ROUTINE_NAME_LEN {
        return Err(format!(
            "Routine name must be at most {MAX_ROUTINE_NAME_LEN} characters (got {}).",
            name.len()
        ));
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err(
            "Routine name must not contain '..' or be a parent-directory reference.".into(),
        );
    }
    for ch in name.chars() {
        if ch == '/' || ch == '\\' {
            return Err(format!(
                "Routine name must not contain path separators ('/' or '\\'); got {ch:?}."
            ));
        }
        if ch == '\0' {
            return Err("Routine name must not contain NUL bytes.".into());
        }
        if ch.is_control() {
            return Err(format!(
                "Routine name must not contain control characters; got {ch:?}."
            ));
        }
        if !ch.is_ascii() || !ch.is_ascii_graphic() {
            return Err(format!(
                "Routine name must be ASCII-printable (letters, digits, '-', '_', '.'); got {ch:?}."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::cron_support::with_test_store;

    fn ctx() -> ToolContext {
        crate::tools::cron_support::test_ctx()
    }

    #[tokio::test]
    async fn create_persists_routine_with_explicit_name() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        let res = tool
            .call(
                json!({
                    "cron_expression": "*/15 * * * *",
                    "prompt": "ping the server",
                    "name": "fifteen-min-ping"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!res.is_error, "create should succeed: {}", res.content);
        assert!(res.content.contains("fifteen-min-ping"));

        let store = open_store().unwrap();
        let loaded = store.load("fifteen-min-ping").unwrap();
        assert_eq!(loaded.cron, "*/15 * * * *");
        assert_eq!(loaded.prompt, "ping the server");
        assert!(loaded.enabled);
    }

    #[tokio::test]
    async fn create_generates_id_when_name_omitted() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        let res = tool
            .call(
                json!({
                    "cron_expression": "0 9 * * *",
                    "prompt": "morning report"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!res.is_error);
        let parsed: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        let id = parsed["id"].as_str().unwrap();
        assert!(id.starts_with("cron-"), "id should start with cron-: {id}");
    }

    #[tokio::test]
    async fn create_rejects_invalid_cron() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        let err = tool
            .call(
                json!({
                    "cron_expression": "not a cron",
                    "prompt": "x"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput(_)),
            "expected InvalidInput, got {err:?}"
        );
    }

    #[tokio::test]
    async fn create_refuses_duplicate_name() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        let _ = tool
            .call(
                json!({
                    "cron_expression": "0 9 * * *",
                    "prompt": "first",
                    "name": "dup"
                }),
                &ctx(),
            )
            .await
            .unwrap();

        let err = tool
            .call(
                json!({
                    "cron_expression": "0 10 * * *",
                    "prompt": "second",
                    "name": "dup"
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    #[test]
    fn validate_routine_name_accepts_normal_id() {
        validate_routine_name("nightly-cleanup").expect("normal id should pass");
        validate_routine_name("cron-abc123").expect("generated id shape should pass");
        validate_routine_name("a.b_c-1").expect("punctuation subset should pass");
    }

    #[test]
    fn validate_routine_name_rejects_empty() {
        let err = validate_routine_name("").unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_rejects_path_separators() {
        let err = validate_routine_name("foo/bar").unwrap_err();
        assert!(err.contains("path separator"), "got: {err}");
        let err = validate_routine_name("foo\\bar").unwrap_err();
        assert!(err.contains("path separator"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_rejects_dot_dot() {
        for bad in ["..", "../etc/passwd", "foo/..", "bad..name"] {
            let err = validate_routine_name(bad).unwrap_err();
            assert!(
                err.contains("'..'") || err.contains("path separator"),
                "{bad}: {err}"
            );
        }
        let err = validate_routine_name(".").unwrap_err();
        assert!(err.contains("'..'") || err.contains("parent"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_rejects_nul_and_control_chars() {
        let err = validate_routine_name("foo\0bar").unwrap_err();
        assert!(err.contains("NUL"), "got: {err}");
        let err = validate_routine_name("foo\nbar").unwrap_err();
        assert!(err.contains("control"), "got: {err}");
        let err = validate_routine_name("foo\tbar").unwrap_err();
        assert!(err.contains("control"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_rejects_non_ascii() {
        let err = validate_routine_name("rénover").unwrap_err();
        assert!(err.contains("ASCII"), "got: {err}");
        let err = validate_routine_name("foo bar").unwrap_err();
        // space is ASCII but not graphic
        assert!(err.contains("ASCII"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_rejects_overlong_input() {
        let long = "a".repeat(MAX_ROUTINE_NAME_LEN + 1);
        let err = validate_routine_name(&long).unwrap_err();
        assert!(err.contains("at most"), "got: {err}");
    }

    #[test]
    fn validate_routine_name_accepts_max_length() {
        let exact = "a".repeat(MAX_ROUTINE_NAME_LEN);
        validate_routine_name(&exact).expect("exact max length should pass");
    }

    #[tokio::test]
    async fn create_rejects_path_traversal_in_name() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        for bad in [
            "../escape",
            "foo/bar",
            "foo\\bar",
            "..",
            "",
            "with space",
            "tab\tname",
        ] {
            let err = tool
                .call(
                    json!({
                        "cron_expression": "0 9 * * *",
                        "prompt": "x",
                        "name": bad,
                    }),
                    &ctx(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, ToolError::InvalidInput(_)),
                "expected InvalidInput for name {bad:?}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn create_returns_next_run_timestamp() {
        let _guard = with_test_store();
        let tool = CronCreateTool;

        let res = tool
            .call(
                json!({
                    "cron_expression": "*/5 * * * *",
                    "prompt": "tick",
                    "name": "every-five"
                }),
                &ctx(),
            )
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&res.content).unwrap();
        let next = parsed["next_run_at"].as_str().expect("next_run_at present");
        assert!(next.ends_with('Z'), "expected ISO timestamp: {next}");
    }
}
