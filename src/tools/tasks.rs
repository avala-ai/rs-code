//! Task management tools.
//!
//! Allow the agent to create, update, and track tasks during
//! execution. Tasks are stored in memory and displayed in the UI.

use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Create a new task for tracking progress.
pub struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn name(&self) -> &'static str {
        "TaskCreate"
    }

    fn description(&self) -> &'static str {
        "Create a task to track progress on a piece of work."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["description"],
            "properties": {
                "description": {
                    "type": "string",
                    "description": "What needs to be done"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "default": "pending"
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true // Tasks are metadata, not file mutations.
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let description = input
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'description' is required".into()))?;

        let status = input
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");

        let id = TASK_COUNTER.fetch_add(1, Ordering::Relaxed);

        Ok(ToolResult::success(format!(
            "Task #{id} created: {description} [{status}]"
        )))
    }
}

/// Update an existing task's status.
pub struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn name(&self) -> &'static str {
        "TaskUpdate"
    }

    fn description(&self) -> &'static str {
        "Update a task's status (pending, in_progress, completed)."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["id", "status"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Task ID to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed"],
                    "description": "New status"
                }
            }
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
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let id = input
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'id' is required".into()))?;

        let status = input
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'status' is required".into()))?;

        Ok(ToolResult::success(format!(
            "Task #{id} updated to [{status}]"
        )))
    }
}
