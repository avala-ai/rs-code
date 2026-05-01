//! RemoteTrigger tool: fire a one-off run of a stored routine.
//!
//! The schedule executor needs an LLM provider and full config to drive
//! a turn, neither of which the per-tool `ToolContext` carries. We mirror
//! the [`crate::tools::agent::AgentTool`] pattern and spawn the host
//! `agent schedule run <id>` subprocess so the routine runs through the
//! same code path as `agent schedule run`. The tool waits for the
//! subprocess to finish (subject to an optional timeout) and returns its
//! captured output, keeping the call request/response in spirit.

use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;
use crate::permissions::{PermissionChecker, PermissionDecision};

use super::cron_support::open_store;

/// Default wall-clock cap for a remote-triggered run. Keeps the tool
/// call from hanging indefinitely if the routine wedges. Callers can
/// raise (or lower) this via `timeout_seconds`.
const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Hard ceiling — even when callers ask for longer, we cap here so a
/// runaway routine can't hold the tool open forever.
const MAX_TIMEOUT_SECS: u64 = 3600;

pub struct RemoteTriggerTool;

#[async_trait]
impl Tool for RemoteTriggerTool {
    fn name(&self) -> &'static str {
        "RemoteTrigger"
    }

    fn description(&self) -> &'static str {
        "Fire a one-off run of a stored cron routine and return its output. \
         Blocks until the routine finishes or the timeout elapses."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Routine id to trigger (as returned by CronCreate or CronList)."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3600,
                    "description": "Optional wall-clock timeout for the run. Defaults to 600 seconds, capped at 3600."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn is_destructive(&self) -> bool {
        // Triggering a run consumes API budget and may mutate the
        // working directory, so gate it on the standard permission
        // checker rather than auto-allowing.
        true
    }

    async fn check_permissions(
        &self,
        input: &serde_json::Value,
        checker: &PermissionChecker,
    ) -> PermissionDecision {
        checker.check(self.name(), input)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let id = input
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'id' is required".into()))?;

        // Verify the routine exists before forking — gives the model a
        // crisp error rather than a subprocess failure code.
        let store = open_store().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to open schedule store: {e}"))
        })?;
        if store.load(id).is_err() {
            return Err(ToolError::InvalidInput(format!(
                "No routine with id '{id}' exists. Use CronList to see available routines."
            )));
        }

        let timeout_secs = input
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS);

        // Spawn `agent schedule run <id>` to delegate to the existing
        // executor. The subprocess inherits provider env vars; the
        // routine record itself supplies cwd, model, and prompt.
        let agent_binary = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "agent".to_string());

        let mut cmd = tokio::process::Command::new(&agent_binary);
        cmd.arg("schedule")
            .arg("run")
            .arg(id)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Defense in depth: if this future is dropped (panic, caller
            // abort) before we explicitly kill the child below, Tokio
            // will reap it for us instead of leaving an orphan.
            .kill_on_drop(true);

        // Forward common provider env vars so the subprocess can
        // authenticate without re-reading config.
        for var in &[
            "AGENT_CODE_API_KEY",
            "AGENT_CODE_API_BASE_URL",
            "AGENT_CODE_MODEL",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "XAI_API_KEY",
            "GOOGLE_API_KEY",
            "DEEPSEEK_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "TOGETHER_API_KEY",
            crate::tools::cron_support::SCHEDULES_DIR_ENV,
        ] {
            if let Ok(val) = std::env::var(var) {
                cmd.env(var, val);
            }
        }

        let outcome = run_with_timeout(cmd, Duration::from_secs(timeout_secs), &ctx.cancel)
            .await
            .map_err(|e| match e {
                RunError::Spawn(msg) => ToolError::ExecutionFailed(format!(
                    "Failed to spawn '{agent_binary}' schedule run {id}: {msg}"
                )),
                RunError::Wait(msg) => ToolError::ExecutionFailed(format!(
                    "Failed waiting on '{agent_binary}' schedule run {id}: {msg}"
                )),
                RunError::Timeout(ms) => ToolError::Timeout(ms),
                RunError::Cancelled => ToolError::Cancelled,
            })?;

        let stdout = String::from_utf8_lossy(&outcome.stdout).to_string();
        let stderr = String::from_utf8_lossy(&outcome.stderr).to_string();
        let success = outcome.status.success();

        let mut content = format!(
            "Routine '{id}' triggered (exit={}).\n",
            outcome
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "?".into())
        );
        if !stdout.is_empty() {
            content.push_str("\n--- stdout ---\n");
            content.push_str(&stdout);
        }
        if !stderr.is_empty() {
            content.push_str("\n--- stderr ---\n");
            content.push_str(&stderr);
        }

        Ok(ToolResult {
            content,
            is_error: !success,
        })
    }
}

/// Outcome of a spawned subprocess that ran to completion.
struct RunOutcome {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Failure modes from [`run_with_timeout`].
#[cfg_attr(test, derive(Debug))]
enum RunError {
    Spawn(String),
    Wait(String),
    /// Timeout in milliseconds.
    Timeout(u64),
    Cancelled,
}

/// Spawn `cmd`, wait for it with a wall-clock timeout, and reap the
/// child on timeout/cancel.
///
/// `cmd` must already be configured with `Stdio::piped()` for stdout
/// and stderr; this helper takes the pipes off the spawned `Child` and
/// drains them concurrently. On timeout or cancel we issue `start_kill`
/// and `wait` on the child before returning so we never leave an
/// orphan agent subprocess running after the tool call has resolved.
async fn run_with_timeout(
    mut cmd: tokio::process::Command,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<RunOutcome, RunError> {
    let mut child = cmd.spawn().map_err(|e| RunError::Spawn(e.to_string()))?;

    let mut stdout_handle = child.stdout.take().expect("stdout piped at spawn");
    let mut stderr_handle = child.stderr.take().expect("stderr piped at spawn");
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();

    let result = tokio::select! {
        r = async {
            let (so, se) = tokio::join!(
                stdout_handle.read_to_end(&mut stdout_buf),
                stderr_handle.read_to_end(&mut stderr_buf),
            );
            so?;
            se?;
            child.wait().await
        } => r,
        _ = tokio::time::sleep(timeout) => {
            // Reap the runaway child so it can't keep consuming API
            // budget after we've already returned to the caller.
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(RunError::Timeout(timeout.as_millis() as u64));
        }
        _ = cancel.cancelled() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(RunError::Cancelled);
        }
    };

    match result {
        Ok(status) => Ok(RunOutcome {
            status,
            stdout: stdout_buf,
            stderr: stderr_buf,
        }),
        Err(e) => Err(RunError::Wait(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::cron_support::{test_ctx, with_test_store};
    use std::time::Instant;

    #[tokio::test]
    async fn trigger_rejects_unknown_routine() {
        let _guard = with_test_store();
        let err = RemoteTriggerTool
            .call(json!({"id": "missing"}), &test_ctx())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::InvalidInput(_)),
            "expected InvalidInput, got {err:?}"
        );
    }

    #[tokio::test]
    async fn trigger_requires_id() {
        let _guard = with_test_store();
        let err = RemoteTriggerTool
            .call(json!({}), &test_ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput(_)));
    }

    /// Build a sleep command that must be killed to terminate. We use
    /// the full path-less `sleep` so the OS resolves it via PATH.
    fn long_sleep_cmd() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    #[tokio::test]
    #[cfg_attr(windows, ignore = "POSIX `sleep` not available on Windows")]
    async fn run_with_timeout_kills_child_on_timeout() {
        let cancel = CancellationToken::new();
        let start = Instant::now();
        let result = run_with_timeout(long_sleep_cmd(), Duration::from_millis(100), &cancel).await;
        let elapsed = start.elapsed();

        match result {
            Err(RunError::Timeout(ms)) => assert_eq!(ms, 100),
            Err(RunError::Cancelled) => panic!("expected Timeout, got Cancelled"),
            Err(RunError::Spawn(msg)) => panic!("expected Timeout, got Spawn({msg})"),
            Err(RunError::Wait(msg)) => panic!("expected Timeout, got Wait({msg})"),
            Ok(_) => panic!("expected Timeout, got Ok"),
        }
        // If the child wasn't reaped, the helper would still be holding
        // the pipes open and waiting. The timeout branch escapes the
        // pipe drain, kills the child, and waits for it to exit before
        // returning, so the helper completes in well under the sleep
        // target. Bounding elapsed at 5s gives plenty of CI headroom
        // while still catching the orphaned-child regression.
        assert!(
            elapsed < Duration::from_secs(5),
            "run_with_timeout should return promptly after killing the child; took {elapsed:?}"
        );
    }

    #[tokio::test]
    #[cfg_attr(windows, ignore = "POSIX `sleep` not available on Windows")]
    async fn run_with_timeout_kills_child_on_cancel() {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_for_task.cancel();
        });

        let start = Instant::now();
        let result = run_with_timeout(long_sleep_cmd(), Duration::from_secs(60), &cancel).await;
        let elapsed = start.elapsed();

        assert!(
            matches!(result, Err(RunError::Cancelled)),
            "expected RunError::Cancelled"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "cancel branch should kill the child promptly; took {elapsed:?}"
        );
    }

    #[tokio::test]
    #[cfg_attr(windows, ignore = "POSIX `true` not available on Windows")]
    async fn run_with_timeout_returns_output_on_success() {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("printf 'hello'; printf 'oops' 1>&2; exit 0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let cancel = CancellationToken::new();
        let outcome = run_with_timeout(cmd, Duration::from_secs(5), &cancel)
            .await
            .expect("command should succeed within timeout");

        assert!(outcome.status.success());
        assert_eq!(outcome.stdout, b"hello");
        assert_eq!(outcome.stderr, b"oops");
    }
}
