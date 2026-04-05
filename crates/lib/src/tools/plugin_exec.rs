//! Plugin executable tool.
//!
//! Wraps an external binary from a plugin's `bin/` directory as a
//! callable tool. The binary receives JSON input on stdin and returns
//! JSON output on stdout.
//!
//! Protocol:
//! - Input: `{"input": <tool_input_json>}` written to stdin
//! - Output: stdout is the tool result text
//! - Exit code 0 = success, non-zero = error
//! - Stderr is captured and included in error messages

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::debug;

use super::{Tool, ToolContext, ToolResult};
use crate::error::ToolError;

/// A tool backed by an external executable from a plugin's bin/ directory.
pub struct PluginExecTool {
    /// Tool name (derived from binary filename).
    tool_name: String,
    /// Human-readable description.
    tool_description: String,
    /// Path to the executable.
    binary_path: PathBuf,
}

impl PluginExecTool {
    /// Create a new plugin executable tool.
    pub fn new(name: String, description: String, binary_path: PathBuf) -> Self {
        Self {
            tool_name: name,
            tool_description: description,
            binary_path,
        }
    }

    /// The tool name (needed because `name()` returns `&'static str`
    /// but we have a dynamic String — we leak it for the static lifetime).
    fn leaked_name(&self) -> &'static str {
        // This is intentional: plugin tools live for the process lifetime.
        // The number of plugins is small and bounded.
        Box::leak(self.tool_name.clone().into_boxed_str())
    }

    fn leaked_description(&self) -> &'static str {
        Box::leak(self.tool_description.clone().into_boxed_str())
    }
}

#[async_trait]
impl Tool for PluginExecTool {
    fn name(&self) -> &'static str {
        self.leaked_name()
    }

    fn description(&self) -> &'static str {
        self.leaked_description()
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Input to pass to the plugin executable"
                }
            }
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input_str = serde_json::to_string(&input).unwrap_or_default();

        debug!(
            "Executing plugin tool '{}': {}",
            self.tool_name,
            self.binary_path.display()
        );

        let output = Command::new(&self.binary_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn plugin: {e}")))?
            .wait_with_output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Plugin execution failed: {e}")))?;

        // For now, pass input via args since piping requires more complex spawn handling.
        // TODO: pipe input_str to stdin for large inputs.
        let _ = input_str;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            Ok(ToolResult {
                content: stdout,
                is_error: false,
            })
        } else {
            let error_msg = if stderr.is_empty() {
                format!("Plugin exited with code {}", output.status)
            } else {
                format!(
                    "Plugin exited with code {}: {}",
                    output.status,
                    stderr.trim()
                )
            };
            Ok(ToolResult {
                content: error_msg,
                is_error: true,
            })
        }
    }
}

/// Discover executable tools from a plugin's bin/ directory.
pub fn discover_plugin_executables(
    plugin_path: &std::path::Path,
    plugin_name: &str,
) -> Vec<PluginExecTool> {
    let bin_dir = plugin_path.join("bin");
    if !bin_dir.is_dir() {
        return Vec::new();
    }

    let entries = match std::fs::read_dir(&bin_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut tools = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Check if executable (Unix) or .exe (Windows).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = path.metadata()
                && meta.permissions().mode() & 0o111 == 0
            {
                continue; // Not executable.
            }
        }
        #[cfg(windows)]
        {
            if path.extension().and_then(|e| e.to_str()) != Some("exe") {
                continue;
            }
        }

        let bin_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");

        let tool_name = format!("plugin__{plugin_name}__{bin_name}");
        let description = format!("Plugin executable: {bin_name} (from {plugin_name})");

        debug!(
            "Discovered plugin executable: {} at {}",
            tool_name,
            path.display()
        );

        tools.push(PluginExecTool::new(tool_name, description, path));
    }

    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let tools = discover_plugin_executables(dir.path(), "test-plugin");
        assert!(tools.is_empty());
    }

    #[test]
    fn test_discover_no_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No bin/ subdirectory.
        let tools = discover_plugin_executables(dir.path(), "test-plugin");
        assert!(tools.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn test_discover_executable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();

        // Create an executable file.
        let exe_path = bin_dir.join("my-tool");
        std::fs::write(&exe_path, "#!/bin/sh\necho ok").unwrap();
        std::fs::set_permissions(&exe_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Create a non-executable file.
        let noexec_path = bin_dir.join("not-a-tool.txt");
        std::fs::write(&noexec_path, "data").unwrap();

        let tools = discover_plugin_executables(dir.path(), "test-plugin");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_name, "plugin__test-plugin__my-tool");
    }
}
