//! Configuration system.
//!
//! Configuration is loaded from multiple sources with the following
//! priority (highest to lowest):
//!
//! 1. CLI flags and environment variables
//! 2. Project-local settings (`.agent/settings.toml`)
//! 3. User settings (`~/.config/agent-code/config.toml`)
//!
//! Each layer is merged into the final `Config` struct.

mod schema;

pub use schema::*;

use crate::error::ConfigError;
use std::path::{Path, PathBuf};

/// Re-entrancy guard to prevent Config::load → log → Config::load cycles.
static LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Config {
    /// Load configuration from all sources, merging by priority.
    pub fn load() -> Result<Config, ConfigError> {
        // Re-entrancy guard.
        if LOADING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Ok(Config::default());
        }
        let result = Self::load_inner();
        LOADING.store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    fn load_inner() -> Result<Config, ConfigError> {
        // Merge config files at the raw `toml::Value` level *before* typed
        // deserialization. If we deserialized each layer into `Config` first,
        // `#[serde(default)]` would synthesize full `ApiConfig::default()` /
        // `UiConfig::default()` / etc. for any section the file omits, and
        // those synthesized defaults would clobber real values from lower
        // layers during merge (see issue #101). Merging raw keeps absent
        // sections absent until the single final `try_into` below.
        let mut merged = toml::Value::Table(toml::value::Table::new());
        // `permissions.rules` has extend-semantics (user rules + project rules
        // concatenated). The recursive table merge would replace the array, so
        // we collect each layer's rules separately and splice them in after.
        let mut all_rules: Vec<toml::Value> = Vec::new();

        // Layer 1: User-level config (lowest priority file).
        if let Some(path) = user_config_path()
            && path.exists()
        {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::FileError(format!("{path:?}: {e}")))?;
            let value: toml::Value = toml::from_str(&content)?;
            collect_permission_rules(&value, &mut all_rules);
            merge_toml_values(&mut merged, &value);
        }

        // Layer 2: Project-level config (overrides user config).
        if let Some(path) = find_project_config() {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::FileError(format!("{path:?}: {e}")))?;
            let value: toml::Value = toml::from_str(&content)?;
            collect_permission_rules(&value, &mut all_rules);
            merge_toml_values(&mut merged, &value);
        }

        if !all_rules.is_empty()
            && let toml::Value::Table(root) = &mut merged
        {
            let perms = root
                .entry("permissions".to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            if let toml::Value::Table(pt) = perms {
                pt.insert("rules".to_string(), toml::Value::Array(all_rules));
            }
        }

        let mut config: Config = merged.try_into()?;

        // Layer 3: Environment variables override file-based config.
        // API key from env always wins over config files, because users
        // expect `OPENAI_API_KEY=x agent` to use key x, even if a
        // stale key exists in config.toml.
        let env_api_key = resolve_api_key_from_env();
        if env_api_key.is_some() {
            config.api.api_key = env_api_key;
        }

        // Base URL from env overrides file config.
        if let Ok(url) = std::env::var("AGENT_CODE_API_BASE_URL") {
            config.api.base_url = url;
        }

        // Model from env overrides file config.
        if let Ok(model) = std::env::var("AGENT_CODE_MODEL") {
            config.api.model = model;
        }

        Ok(config)
    }
}

/// Recursively merge `overlay` into `base`. Tables merge key-by-key; any
/// non-table value in `overlay` replaces the value in `base`. Adapted from
/// openai/codex's `merge_toml_values`.
fn merge_toml_values(base: &mut toml::Value, overlay: &toml::Value) {
    if let toml::Value::Table(overlay_table) = overlay
        && let toml::Value::Table(base_table) = base
    {
        for (key, value) in overlay_table {
            if let Some(existing) = base_table.get_mut(key) {
                merge_toml_values(existing, value);
            } else {
                base_table.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overlay.clone();
    }
}

fn collect_permission_rules(value: &toml::Value, out: &mut Vec<toml::Value>) {
    if let Some(rules) = value
        .get("permissions")
        .and_then(|p| p.get("rules"))
        .and_then(|r| r.as_array())
    {
        out.extend(rules.iter().cloned());
    }
}

/// Resolve API key from environment variables.
///
/// Checks each provider's env var in priority order. Returns the first
/// one found, or None if no API key is set in the environment.
fn resolve_api_key_from_env() -> Option<String> {
    std::env::var("AGENT_CODE_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .or_else(|_| std::env::var("XAI_API_KEY"))
        .or_else(|_| std::env::var("GOOGLE_API_KEY"))
        .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
        .or_else(|_| std::env::var("GROQ_API_KEY"))
        .or_else(|_| std::env::var("MISTRAL_API_KEY"))
        .or_else(|_| std::env::var("ZHIPU_API_KEY"))
        .or_else(|_| std::env::var("TOGETHER_API_KEY"))
        .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
        .or_else(|_| std::env::var("COHERE_API_KEY"))
        .or_else(|_| std::env::var("PERPLEXITY_API_KEY"))
        .ok()
}

/// Returns the user-level config file path.
fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-code").join("config.toml"))
}

/// Walk up from the current directory to find `.agent/settings.toml`.
fn find_project_config() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_config_in_ancestors(&cwd)
}

/// Watch config files for changes and reload when modified.
/// Returns a handle that can be dropped to stop watching.
pub fn watch_config(
    on_reload: impl Fn(Config) + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let user_path = user_config_path()?;
    let project_path = find_project_config();

    // Get initial mtimes.
    let user_mtime = std::fs::metadata(&user_path)
        .ok()
        .and_then(|m| m.modified().ok());
    let project_mtime = project_path
        .as_ref()
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok());

    Some(std::thread::spawn(move || {
        let mut last_user = user_mtime;
        let mut last_project = project_mtime;

        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));

            let cur_user = std::fs::metadata(&user_path)
                .ok()
                .and_then(|m| m.modified().ok());
            let cur_project = project_path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok());

            let changed = cur_user != last_user || cur_project != last_project;

            if changed {
                if let Ok(config) = Config::load() {
                    tracing::info!("Config reloaded (file change detected)");
                    on_reload(config);
                }
                last_user = cur_user;
                last_project = cur_project;
            }
        }
    }))
}

fn find_config_in_ancestors(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".agent").join("settings.toml");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod merge_tests {
    use super::*;

    /// Helper: simulate load_inner's merge pipeline for two layers, returning
    /// the final typed Config.
    fn merge_layers(user: &str, project: &str) -> Config {
        let mut merged = toml::Value::Table(toml::value::Table::new());
        let mut all_rules: Vec<toml::Value> = Vec::new();

        for layer in [user, project] {
            if layer.is_empty() {
                continue;
            }
            let v: toml::Value = toml::from_str(layer).unwrap();
            collect_permission_rules(&v, &mut all_rules);
            merge_toml_values(&mut merged, &v);
        }

        if !all_rules.is_empty()
            && let toml::Value::Table(root) = &mut merged
        {
            let perms = root
                .entry("permissions".to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            if let toml::Value::Table(pt) = perms {
                pt.insert("rules".to_string(), toml::Value::Array(all_rules));
            }
        }

        merged.try_into().unwrap()
    }

    // ---- Issue #101: project config without [api] must not clobber user api ----

    #[test]
    fn project_without_api_section_preserves_user_base_url_and_model() {
        let user = r#"
[api]
base_url = "http://localhost:11434/v1"
model = "gemma4:26b"
"#;
        let project = r#"
[mcp_servers.my-server]
command = "/usr/local/bin/my-mcp"
args = []
"#;
        let cfg = merge_layers(user, project);
        assert_eq!(cfg.api.base_url, "http://localhost:11434/v1");
        assert_eq!(cfg.api.model, "gemma4:26b");
        assert!(cfg.mcp_servers.contains_key("my-server"));
    }

    #[test]
    fn project_partial_api_only_overrides_specified_fields() {
        let user = r#"
[api]
base_url = "http://localhost:11434/v1"
model = "gemma4:26b"
"#;
        let project = r#"
[api]
model = "llama3:70b"
"#;
        let cfg = merge_layers(user, project);
        // Project overrides model.
        assert_eq!(cfg.api.model, "llama3:70b");
        // base_url is inherited from user, not clobbered by default.
        assert_eq!(cfg.api.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn project_without_ui_section_preserves_user_theme() {
        let user = r#"
[ui]
theme = "solarized"
edit_mode = "vi"
"#;
        let project = r#"
[mcp_servers.foo]
command = "x"
"#;
        let cfg = merge_layers(user, project);
        assert_eq!(cfg.ui.theme, "solarized");
        assert_eq!(cfg.ui.edit_mode, "vi");
    }

    #[test]
    fn project_without_features_preserves_user_feature_flags() {
        let user = r#"
[features]
token_budget = false
prompt_caching = false
"#;
        let project = "";
        let cfg = merge_layers(user, project);
        assert!(!cfg.features.token_budget);
        assert!(!cfg.features.prompt_caching);
        // Unspecified flags fall back to their struct default (true).
        assert!(cfg.features.commit_attribution);
    }

    #[test]
    fn permission_rules_extend_across_layers() {
        let user = r#"
[[permissions.rules]]
tool = "Read"
action = "allow"

[[permissions.rules]]
tool = "Bash"
pattern = "rm -rf *"
action = "deny"
"#;
        let project = r#"
[[permissions.rules]]
tool = "Write"
action = "ask"
"#;
        let cfg = merge_layers(user, project);
        assert_eq!(cfg.permissions.rules.len(), 3);
        assert_eq!(cfg.permissions.rules[0].tool, "Read");
        assert_eq!(cfg.permissions.rules[1].tool, "Bash");
        assert_eq!(cfg.permissions.rules[2].tool, "Write");
    }

    #[test]
    fn mcp_servers_merge_by_name_project_overrides_user() {
        let user = r#"
[mcp_servers.alpha]
command = "user-alpha"

[mcp_servers.beta]
command = "user-beta"
"#;
        let project = r#"
[mcp_servers.beta]
command = "project-beta"

[mcp_servers.gamma]
command = "project-gamma"
"#;
        let cfg = merge_layers(user, project);
        assert_eq!(
            cfg.mcp_servers["alpha"].command.as_deref(),
            Some("user-alpha")
        );
        assert_eq!(
            cfg.mcp_servers["beta"].command.as_deref(),
            Some("project-beta")
        );
        assert_eq!(
            cfg.mcp_servers["gamma"].command.as_deref(),
            Some("project-gamma")
        );
    }

    #[test]
    fn no_layers_yields_default_config() {
        let cfg = merge_layers("", "");
        assert_eq!(cfg.api.model, "gpt-5.4");
        assert_eq!(cfg.permissions.default_mode, PermissionMode::Ask);
    }

    // ---- merge_toml_values primitive ----

    #[test]
    fn merge_toml_values_recursive_table_merge() {
        let mut base: toml::Value = toml::from_str(
            r#"
[api]
base_url = "http://a"
model = "m1"
"#,
        )
        .unwrap();
        let overlay: toml::Value = toml::from_str(
            r#"
[api]
model = "m2"
"#,
        )
        .unwrap();
        merge_toml_values(&mut base, &overlay);
        let api = base.get("api").unwrap();
        assert_eq!(api.get("base_url").unwrap().as_str(), Some("http://a"));
        assert_eq!(api.get("model").unwrap().as_str(), Some("m2"));
    }

    #[test]
    fn merge_toml_values_overlay_replaces_non_table() {
        let mut base = toml::Value::String("old".into());
        let overlay = toml::Value::String("new".into());
        merge_toml_values(&mut base, &overlay);
        assert_eq!(base.as_str(), Some("new"));
    }
}
