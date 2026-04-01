//! Plan mode tools: switch between execution and planning modes.
//!
//! Plan mode restricts the agent to read-only tools, preventing
//! mutations while the user reviews and approves a plan.
//! The LLM decides when to enter plan mode based on task complexity.

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

/// Enter plan mode (read-only operations only).
pub struct EnterPlanModeTool;

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &'static str {
        "EnterPlanMode"
    }

    fn description(&self) -> &'static str {
        "Switch to plan mode for safe exploration before making changes."
    }

    fn prompt(&self) -> String {
        "Use this tool when you need to plan an approach before making changes. \
         In plan mode, only read-only tools are available (FileRead, Grep, Glob, Bash). \
         Write tools are blocked until ExitPlanMode is called.\n\n\
         When to enter plan mode:\n\
         - Complex tasks requiring multiple file changes\n\
         - Unclear requirements that need investigation first\n\
         - Multiple possible approaches to evaluate\n\
         - Large refactors where the plan should be reviewed\n\
         - When the user asks to \"plan\", \"think through\", or \"design\"\n\n\
         You should write your plan to a file before exiting plan mode."
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Generate a plan file path.
        let plan_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("agent-code")
            .join("plans");
        let _ = std::fs::create_dir_all(&plan_dir);

        let slug = generate_slug();
        let plan_path = plan_dir.join(format!("{slug}.md"));

        // Create the plan file with a template.
        let template = format!(
            "# Plan\n\n\
             Created: {}\n\n\
             ## Goal\n\n\
             (describe what needs to be accomplished)\n\n\
             ## Approach\n\n\
             (outline the steps)\n\n\
             ## Files to modify\n\n\
             (list files and what changes each needs)\n\n\
             ## Risks / open questions\n\n\
             (anything uncertain)\n",
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"),
        );
        let _ = std::fs::write(&plan_path, &template);

        Ok(ToolResult::success(format!(
            "Entered plan mode. Only read-only tools are available.\n\
             Plan file created: {}\n\
             Write your plan to this file, then call ExitPlanMode when ready.",
            plan_path.display()
        )))
    }
}

/// Exit plan mode (re-enable all tools).
pub struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &'static str {
        "ExitPlanMode"
    }

    fn description(&self) -> &'static str {
        "Exit plan mode and re-enable all tools for execution. \
         Call this after your plan is complete and ready to implement."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success(
            "Exited plan mode. All tools are now available for execution.",
        ))
    }
}

/// Generate a memorable slug for plan files (adjective-noun).
fn generate_slug() -> String {
    let adjectives = [
        "brave", "calm", "dark", "eager", "fair", "golden", "hidden", "iron", "jade", "keen",
        "light", "mystic", "noble", "ocean", "proud", "quick", "rapid", "silent", "true", "vivid",
    ];
    let nouns = [
        "anchor", "beacon", "cedar", "dawn", "ember", "falcon", "grove", "harbor", "island",
        "jewel", "kernel", "lantern", "meadow", "nexus", "orbit", "peak", "quill", "river",
        "spark", "tower",
    ];

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();

    let adj = adjectives[(now as usize) % adjectives.len()];
    let noun = nouns[((now as usize) / adjectives.len()) % nouns.len()];

    format!("{adj}-{noun}")
}
