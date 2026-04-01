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
        let mut config = Config::default();

        // Layer 1: User-level config.
        if let Some(path) = user_config_path()
            && path.exists()
        {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::FileError(format!("{path:?}: {e}")))?;
            let user_config: Config = toml::from_str(&content)?;
            config.merge(user_config);
        }

        // Layer 2: Project-level config (walk up from cwd).
        if let Some(path) = find_project_config() {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| ConfigError::FileError(format!("{path:?}: {e}")))?;
            let project_config: Config = toml::from_str(&content)?;
            config.merge(project_config);
        }

        // Layer 3: Environment variables (applied in CLI parsing).

        Ok(config)
    }

    /// Merge another config into this one. Non-default values from `other`
    /// overwrite values in `self`.
    fn merge(&mut self, other: Config) {
        if !other.api.base_url.is_empty() {
            self.api.base_url = other.api.base_url;
        }
        if !other.api.model.is_empty() {
            self.api.model = other.api.model;
        }
        if other.api.api_key.is_some() {
            self.api.api_key = other.api.api_key;
        }
        if other.api.max_output_tokens.is_some() {
            self.api.max_output_tokens = other.api.max_output_tokens;
        }
        if other.permissions.default_mode != PermissionMode::Ask {
            self.permissions.default_mode = other.permissions.default_mode;
        }
        if !other.permissions.rules.is_empty() {
            self.permissions.rules.extend(other.permissions.rules);
        }
        // MCP servers merge by name (project overrides user).
        for (name, entry) in other.mcp_servers {
            self.mcp_servers.insert(name, entry);
        }
    }
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
