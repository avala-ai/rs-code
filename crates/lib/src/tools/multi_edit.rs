//! MultiEdit tool: batch search-and-replace across multiple locations in one file.
//!
//! Accepts an array of `{old_string, new_string}` pairs and applies them
//! sequentially to a single file. Each pair performs an exact match
//! replacement (one occurrence). The tool rejects the entire batch if
//! any individual edit would fail (missing match, ambiguous match, or
//! identity replacement), ensuring atomicity.
//!
//! Shares the same staleness guard as `FileEdit` — if the file changed
//! since the model last read it, the batch is rejected until a fresh
//! read is performed.

use async_trait::async_trait;
use serde_json::json;
use similar::TextDiff;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

pub struct MultiEditTool;

/// Verify that the file has not been modified since the cache last recorded it.
///
/// Returns `Ok(())` when the cache has no entry or the mtimes still agree.
/// Returns a descriptive error when the file is stale.
async fn check_staleness(path: &Path, ctx: &ToolContext) -> Result<(), String> {
    let cache = match ctx.file_cache.as_ref() {
        Some(c) => c,
        None => return Ok(()),
    };

    let cached_mtime: SystemTime = {
        let guard = cache.lock().await;
        match guard.last_read_mtime(path) {
            Some(t) => t,
            None => return Ok(()),
        }
    };

    let disk_mtime = tokio::fs::metadata(path)
        .await
        .ok()
        .and_then(|m| m.modified().ok());

    if let Some(disk) = disk_mtime
        && disk != cached_mtime
    {
        return Err(format!(
            "File changed on disk since last read. \
             Re-read {} before editing.",
            path.display()
        ));
    }

    Ok(())
}

/// Build a unified diff between two versions of the same file.
fn unified_diff(file_path: &str, before: &str, after: &str) -> String {
    let diff = TextDiff::from_lines(before, after);
    let mut out = String::new();

    out.push_str(&format!("--- {file_path}\n"));
    out.push_str(&format!("+++ {file_path}\n"));

    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        out.push_str(&format!("{hunk}"));
    }

    if out.lines().count() <= 2 {
        out.push_str("(no visible changes)\n");
    }

    out
}

/// Represents a single search-and-replace pair extracted from the input.
struct EditPair {
    old_string: String,
    new_string: String,
}

/// Parse and validate the `edits` array from the tool input.
fn parse_edits(input: &serde_json::Value) -> Result<Vec<EditPair>, ToolError> {
    let edits_val = input
        .get("edits")
        .ok_or_else(|| ToolError::InvalidInput("'edits' array is required".into()))?;

    let edits_arr = edits_val
        .as_array()
        .ok_or_else(|| ToolError::InvalidInput("'edits' must be an array".into()))?;

    if edits_arr.is_empty() {
        return Err(ToolError::InvalidInput(
            "'edits' array must contain at least one entry".into(),
        ));
    }

    let mut pairs = Vec::with_capacity(edits_arr.len());

    for (idx, entry) in edits_arr.iter().enumerate() {
        let old = entry
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidInput(format!("edits[{idx}]: 'old_string' is required"))
            })?;

        let new = entry
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidInput(format!("edits[{idx}]: 'new_string' is required"))
            })?;

        if old == new {
            return Err(ToolError::InvalidInput(format!(
                "edits[{idx}]: old_string and new_string are identical"
            )));
        }

        pairs.push(EditPair {
            old_string: old.to_owned(),
            new_string: new.to_owned(),
        });
    }

    Ok(pairs)
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &'static str {
        "MultiEdit"
    }

    fn description(&self) -> &'static str {
        "Apply multiple search-and-replace edits to a single file in one operation. \
         Each edit must match exactly once. All edits are applied sequentially."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["file_path", "edits"],
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to modify"
                },
                "edits": {
                    "type": "array",
                    "description": "Ordered list of search-and-replace pairs to apply",
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": {
                                "type": "string",
                                "description": "Exact text to find (must match uniquely)"
                            },
                            "new_string": {
                                "type": "string",
                                "description": "Replacement text"
                            }
                        }
                    }
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn get_path(&self, input: &serde_json::Value) -> Option<PathBuf> {
        input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let file_path = input
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'file_path' is required".into()))?;

        let edits = parse_edits(&input)?;

        let path = Path::new(file_path);

        // Reject oversized files.
        const MAX_EDIT_SIZE: u64 = 1_048_576;
        if let Ok(meta) = tokio::fs::metadata(file_path).await
            && meta.len() > MAX_EDIT_SIZE
        {
            return Err(ToolError::InvalidInput(format!(
                "File too large for editing ({} bytes, limit {}). \
                 Use Bash with sed/awk for large files.",
                meta.len(),
                MAX_EDIT_SIZE
            )));
        }

        // Guard against stale content.
        if let Err(msg) = check_staleness(path, ctx).await {
            return Err(ToolError::ExecutionFailed(msg));
        }

        let original = tokio::fs::read_to_string(file_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read {file_path}: {e}")))?;

        // Apply each edit sequentially, accumulating the content.
        let mut content = original.clone();
        let mut applied = 0usize;

        for (idx, pair) in edits.iter().enumerate() {
            let occurrences = content.matches(&pair.old_string).count();

            if occurrences == 0 {
                return Err(ToolError::InvalidInput(format!(
                    "edits[{idx}]: old_string not found in {file_path} \
                     (may have been consumed by a prior edit in this batch)"
                )));
            }

            if occurrences > 1 {
                return Err(ToolError::InvalidInput(format!(
                    "edits[{idx}]: old_string matches {occurrences} locations in {file_path}. \
                     Provide a more specific snippet."
                )));
            }

            content = content.replacen(&pair.old_string, &pair.new_string, 1);
            applied += 1;
        }

        // Write the result.
        tokio::fs::write(file_path, &content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot write {file_path}: {e}")))?;

        // Invalidate the cache so the next read picks up our changes.
        if let Some(cache) = ctx.file_cache.as_ref() {
            let mut guard = cache.lock().await;
            guard.invalidate(path);
        }

        let diff = unified_diff(file_path, &original, &content);
        Ok(ToolResult::success(format!(
            "Applied {applied} edit(s) to {file_path}\n\n{diff}"
        )))
    }
}
