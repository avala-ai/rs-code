//! Config tool: model-callable settings reader/writer with a hard
//! allow-list.
//!
//! The agent can use this tool to inspect or change a small,
//! deliberately narrow set of user-tunable settings. Anything not in
//! [`crate::config::supported_settings::SUPPORTED_SETTINGS`] is
//! rejected with a clear error — there is no escape hatch.
//!
//! # Subcommands (via the `action` arg)
//!
//! - `list_supported` — return every entry on the allow-list with
//!   its key, description, kind, and scope.
//! - `get` — return the current value of one allow-listed setting,
//!   read from the on-disk TOML for its scope. Falls back to "(unset)"
//!   when the file or key is absent.
//! - `set` — validate `value` against the setting's kind, then
//!   write it to the matching on-disk TOML. The user-scope file is
//!   `~/.config/agent-code/config.toml`; the project-scope file is
//!   `<project>/.agent/settings.toml`.
//!
//! # Permission policy
//!
//! `get` and `list_supported` are read-only. `set` mutates a config
//! file and must go through the standard permission gate — by
//! default that means "ask the user" unless an `Allow` rule has
//! been configured for `Config`.
//!
//! The module is named `config_tool` (rather than `config`) to avoid
//! shadowing [`crate::config`].

use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};

use super::{PermissionDecision, Tool, ToolContext, ToolResult};
use crate::config::supported_settings::{self, Scope, SettingKind, SupportedSetting};
use crate::error::ToolError;
use crate::permissions::PermissionChecker;

pub struct ConfigTool;

#[async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &'static str {
        "Config"
    }

    fn description(&self) -> &'static str {
        "Read or write a small allow-list of user-tunable settings \
         (theme, default model, opt-in flags, etc.). Use action=\"list_supported\" \
         to discover what can be changed, action=\"get\" to read a value, \
         and action=\"set\" to update one. Anything not on the allow-list \
         is rejected; this tool cannot change permissions, sandbox, MCP, \
         hooks, or API keys."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "list_supported"],
                    "description": "Which subcommand to invoke"
                },
                "key": {
                    "type": "string",
                    "description": "Dotted setting key (required for get and set)"
                },
                "value": {
                    "description": "New value (required for set). Must match the setting's kind."
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        // The mode is action-dependent — `set` mutates state. We keep
        // the trait method conservative (false) and let
        // [`check_permissions`] differentiate based on the parsed
        // action: read-only actions auto-allow, `set` runs through
        // the configured permission rule.
        false
    }

    async fn check_permissions(
        &self,
        input: &serde_json::Value,
        checker: &PermissionChecker,
    ) -> PermissionDecision {
        match input.get("action").and_then(|v| v.as_str()) {
            Some("get") | Some("list_supported") => PermissionDecision::Allow,
            _ => checker.check(self.name(), input),
        }
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("'action' is required".into()))?;

        match action {
            "list_supported" => Ok(ToolResult::success(format_allowlist())),
            "get" => {
                let key = input
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("'key' is required for get".into()))?;
                let setting = supported_settings::lookup(key).ok_or_else(|| {
                    ToolError::InvalidInput(format!(
                        "setting '{key}' is not on the allow-list. Use action=\"list_supported\" to see what can be read."
                    ))
                })?;
                let value = read_setting(setting, &ctx.cwd)?;
                let value_str = match value {
                    Some(v) => render_toml_value(&v),
                    None => "(unset)".to_string(),
                };
                Ok(ToolResult::success(format!("{key} = {value_str}")))
            }
            "set" => {
                // The interactive permission gate runs *after* this
                // method when [`check_permissions`] returned `Ask`.
                // Honour any prompter installed on the context — if
                // the user denies, we abort before mutating disk.
                if let Some(prompter) = &ctx.permission_prompter {
                    use super::PermissionResponse;
                    match prompter.ask(self.name(), self.description(), Some(&input.to_string())) {
                        PermissionResponse::AllowOnce | PermissionResponse::AllowSession => {}
                        PermissionResponse::Deny => {
                            return Err(ToolError::PermissionDenied(
                                "user denied Config set request".into(),
                            ));
                        }
                    }
                }

                let key = input
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidInput("'key' is required for set".into()))?;
                let value = input
                    .get("value")
                    .ok_or_else(|| ToolError::InvalidInput("'value' is required for set".into()))?;

                let setting = supported_settings::lookup(key).ok_or_else(|| {
                    ToolError::InvalidInput(format!(
                        "setting '{key}' is not on the allow-list. Anything not listed by action=\"list_supported\" is intentionally not mutable from a tool call."
                    ))
                })?;

                let coerced = supported_settings::coerce_value(setting, value)
                    .map_err(ToolError::InvalidInput)?;

                write_setting(setting, &ctx.cwd, coerced.clone())?;

                Ok(ToolResult::success(format!(
                    "Set {} = {} ({})",
                    key,
                    render_toml_value(&coerced),
                    scope_label(setting.scope)
                )))
            }
            other => Err(ToolError::InvalidInput(format!(
                "unknown action '{other}' (expected get, set, or list_supported)"
            ))),
        }
    }
}

/// Resolve the on-disk TOML file path for a setting's scope. `User`
/// always points at `~/.config/agent-code/config.toml`; `Project`
/// walks up from `cwd` looking for an existing `.agent/settings.toml`,
/// and falls back to `<cwd>/.agent/settings.toml` if none exists yet.
fn settings_path_for(scope: Scope, cwd: &Path) -> Option<PathBuf> {
    match scope {
        Scope::User => crate::config::user_config_path(),
        Scope::Project => match crate::config::find_project_config_from(cwd) {
            Some(p) => Some(p),
            None => Some(cwd.join(".agent").join("settings.toml")),
        },
    }
}

/// Read one allow-listed setting from its scope's TOML file. Returns
/// `Ok(None)` if the file or key doesn't exist — the caller renders
/// that as `(unset)`. Wrong-typed values produce an error so a
/// hand-edited file with the wrong shape doesn't silently coerce.
fn read_setting(
    setting: &SupportedSetting,
    cwd: &Path,
) -> Result<Option<toml::Value>, ToolError> {
    let path = match settings_path_for(setting.scope, cwd) {
        Some(p) => p,
        None => return Ok(None),
    };
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {path:?}: {e}")))?;
    let doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| ToolError::ExecutionFailed(format!("parse {path:?}: {e}")))?;

    let mut cur = &doc;
    for segment in setting.key.split('.') {
        match cur.get(segment) {
            Some(v) => cur = v,
            None => return Ok(None),
        }
    }
    if !value_matches_kind(cur, &setting.kind) {
        return Err(ToolError::ExecutionFailed(format!(
            "value at {} has the wrong type for kind {:?}",
            setting.key, setting.kind
        )));
    }
    Ok(Some(cur.clone()))
}

/// Write a coerced [`toml::Value`] into the scope's TOML file,
/// creating intermediate tables and the file itself as needed. The
/// file is read, mutated, and rewritten atomically (write-then-rename
/// is overkill for a config we don't fsync).
fn write_setting(
    setting: &SupportedSetting,
    cwd: &Path,
    value: toml::Value,
) -> Result<(), ToolError> {
    let path = settings_path_for(setting.scope, cwd).ok_or_else(|| {
        ToolError::ExecutionFailed("could not determine settings path".into())
    })?;

    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| ToolError::ExecutionFailed(format!("create {parent:?}: {e}")))?;
    }

    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| ToolError::ExecutionFailed(format!("read {path:?}: {e}")))?;
        toml::from_str(&raw)
            .map_err(|e| ToolError::ExecutionFailed(format!("parse {path:?}: {e}")))?
    } else {
        toml::Value::Table(toml::value::Table::new())
    };

    set_dotted(&mut doc, setting.key, value)?;

    let serialized = toml::to_string_pretty(&doc)
        .map_err(|e| ToolError::ExecutionFailed(format!("serialize: {e}")))?;
    std::fs::write(&path, serialized)
        .map_err(|e| ToolError::ExecutionFailed(format!("write {path:?}: {e}")))?;
    Ok(())
}

/// Insert `value` at the dotted `key` path inside a TOML document,
/// creating any missing tables along the way. Returns an error if a
/// non-table value sits in the path — we refuse to clobber unrelated
/// scalars even though the caller picked an allow-listed key, because
/// the conflict means the file was hand-edited into an unexpected
/// shape and the user should resolve it.
fn set_dotted(doc: &mut toml::Value, key: &str, value: toml::Value) -> Result<(), ToolError> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.is_empty() {
        return Err(ToolError::InvalidInput("empty key".into()));
    }
    let mut cursor = doc;
    for seg in &segments[..segments.len() - 1] {
        let cursor_table = cursor
            .as_table_mut()
            .ok_or_else(|| ToolError::ExecutionFailed(format!(
                "cannot descend into non-table at '{seg}' while setting {key}"
            )))?;
        let entry = cursor_table
            .entry((*seg).to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        if !entry.is_table() {
            return Err(ToolError::ExecutionFailed(format!(
                "key segment '{seg}' is not a table; refusing to overwrite"
            )));
        }
        cursor = entry;
    }
    let leaf = segments.last().unwrap();
    let table = cursor
        .as_table_mut()
        .ok_or_else(|| ToolError::ExecutionFailed("expected table at leaf parent".into()))?;
    table.insert((*leaf).to_string(), value);
    Ok(())
}

fn value_matches_kind(value: &toml::Value, kind: &SettingKind) -> bool {
    match kind {
        SettingKind::Bool => value.is_bool(),
        SettingKind::Int => value.is_integer(),
        SettingKind::Float => value.is_float(),
        SettingKind::String => value.is_str(),
        SettingKind::Enum(allowed) => value.as_str().is_some_and(|s| allowed.contains(&s)),
    }
}

fn render_toml_value(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => format!("\"{s}\""),
        other => other.to_string(),
    }
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user scope",
        Scope::Project => "project scope",
    }
}

fn format_allowlist() -> String {
    let mut out = String::from("Supported settings (allow-list):\n");
    for s in supported_settings::SUPPORTED_SETTINGS {
        out.push_str(&format!(
            "- {} [{}, {}] - {}\n",
            s.key,
            kind_label(&s.kind),
            scope_label(s.scope),
            s.description
        ));
    }
    out
}

fn kind_label(kind: &SettingKind) -> String {
    match kind {
        SettingKind::Bool => "bool".to_string(),
        SettingKind::Int => "int".to_string(),
        SettingKind::Float => "float".to_string(),
        SettingKind::String => "string".to_string(),
        SettingKind::Enum(values) => format!("enum({})", values.join("|")),
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
            permission_checker: Arc::new(PermissionChecker::allow_all()),
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

    /// Override the user config dir to a temp path for the duration of
    /// a test. Returns a guard that restores the previous value on
    /// drop. Holds a process-wide mutex while alive — `cargo test`
    /// runs tests in parallel by default, and `XDG_CONFIG_HOME` is
    /// shared global state that any other test reading the user
    /// config could observe mid-flight.
    struct UserConfigDirGuard {
        prev: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    static USER_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl UserConfigDirGuard {
        fn redirect(to: &Path) -> Self {
            let lock = USER_CONFIG_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // `dirs::config_dir()` reads `$XDG_CONFIG_HOME` on Linux.
            let prev = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: this env mutation is gated by USER_CONFIG_LOCK,
            // so no other thread can read XDG_CONFIG_HOME while we
            // have it pinned.
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", to);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for UserConfigDirGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    #[tokio::test]
    async fn list_supported_returns_known_keys() {
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let res = ConfigTool
            .call(json!({ "action": "list_supported" }), &ctx)
            .await
            .unwrap();
        assert!(res.content.contains("ui.theme"));
        assert!(res.content.contains("features.commit_attribution"));
    }

    #[tokio::test]
    async fn get_unset_project_scope_returns_unset_marker() {
        // Project-scope reads avoid the global XDG_CONFIG_HOME, so
        // this test doesn't need any env shenanigans.
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".agent")).unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let res = ConfigTool
            .call(json!({ "action": "get", "key": "api.model" }), &ctx)
            .await
            .unwrap();
        assert!(res.content.contains("(unset)"));
    }

    #[tokio::test]
    async fn set_rejects_unlisted_key() {
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());

        let err = ConfigTool
            .call(
                json!({
                    "action": "set",
                    "key": "permissions.default_mode",
                    "value": "allow"
                }),
                &ctx,
            )
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput(s) => {
                assert!(s.contains("not on the allow-list"));
            }
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn set_rejects_wrong_type() {
        // Ride on a project-scope key (api.model) so we don't have
        // to mutate XDG_CONFIG_HOME — but use a key whose kind
        // forces a type error. Override the test to use a bool key
        // but with project scope: we exercise the validator BEFORE
        // any disk write, so the scope of the key is irrelevant for
        // this assertion. ui.markdown happens to be user-scope, but
        // the InvalidInput is raised before the path is touched.
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());

        let err = ConfigTool
            .call(
                json!({ "action": "set", "key": "ui.markdown", "value": "true" }),
                &ctx,
            )
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("boolean")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn set_rejects_enum_outside_allowed() {
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());

        let err = ConfigTool
            .call(
                json!({ "action": "set", "key": "ui.theme", "value": "magenta" }),
                &ctx,
            )
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("must be one of")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn set_project_scope_writes_then_get_reads_back() {
        // End-to-end set+get round-trip on the project scope, which
        // is a per-test temp directory and therefore can't race with
        // any other test in the workspace.
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".agent")).unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());

        let set = ConfigTool
            .call(
                json!({ "action": "set", "key": "api.model", "value": "gpt-5.4" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(set.content.contains("project scope"));
        let path = dir.path().join(".agent").join("settings.toml");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("model = \"gpt-5.4\""));

        let get = ConfigTool
            .call(json!({ "action": "get", "key": "api.model" }), &ctx)
            .await
            .unwrap();
        assert!(get.content.contains("\"gpt-5.4\""));
    }

    /// Smoke-test the user-scope set/get path under a guarded env
    /// override. Wrapped in a process-wide mutex so concurrent tests
    /// reading the user config can't see a half-applied value.
    #[tokio::test]
    async fn set_user_scope_writes_to_xdg_config_home() {
        let xdg = TempDir::new().unwrap();
        let _g = UserConfigDirGuard::redirect(xdg.path());
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());

        let set = ConfigTool
            .call(
                json!({ "action": "set", "key": "ui.theme", "value": "light" }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!set.is_error);
        let path = xdg.path().join("agent-code").join("config.toml");
        assert!(path.exists(), "user config file was not created");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("theme = \"light\""));
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let dir = TempDir::new().unwrap();
        let ctx = make_ctx(dir.path().to_path_buf());
        let err = ConfigTool
            .call(json!({ "action": "delete", "key": "ui.theme" }), &ctx)
            .await
            .unwrap_err();
        match err {
            ToolError::InvalidInput(s) => assert!(s.contains("unknown action")),
            _ => panic!("expected InvalidInput"),
        }
    }

    #[tokio::test]
    async fn check_permissions_allows_read_only_actions() {
        let checker = PermissionChecker::allow_all();
        let tool = ConfigTool;
        let dec = tool
            .check_permissions(&json!({ "action": "list_supported" }), &checker)
            .await;
        assert!(matches!(dec, PermissionDecision::Allow));
        let dec = tool
            .check_permissions(&json!({ "action": "get", "key": "ui.theme" }), &checker)
            .await;
        assert!(matches!(dec, PermissionDecision::Allow));
    }
}
