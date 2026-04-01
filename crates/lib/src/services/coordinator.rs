//! Multi-agent coordinator.
//!
//! Routes tasks to specialized agents based on the task type.
//! The coordinator acts as an orchestrator, spawning agents with
//! appropriate configurations and aggregating their results.
//!
//! # Agent types
//!
//! - `general-purpose`: default agent with full tool access
//! - `explore`: fast read-only agent for codebase exploration
//! - `plan`: planning agent restricted to analysis tools
//!
//! Agents are defined as configurations that customize the tool
//! set, system prompt, and permission mode.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Definition of a specialized agent type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique agent type name.
    pub name: String,
    /// Description of what this agent specializes in.
    pub description: String,
    /// System prompt additions for this agent type.
    pub system_prompt: Option<String>,
    /// Model override (if different from default).
    pub model: Option<String>,
    /// Tools to include (if empty, use all).
    pub include_tools: Vec<String>,
    /// Tools to exclude.
    pub exclude_tools: Vec<String>,
    /// Whether this agent runs in read-only mode.
    pub read_only: bool,
    /// Maximum turns for this agent type.
    pub max_turns: Option<usize>,
}

/// Registry of available agent types.
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Create the registry with built-in agent types.
    pub fn with_defaults() -> Self {
        let mut agents = HashMap::new();

        agents.insert(
            "general-purpose".to_string(),
            AgentDefinition {
                name: "general-purpose".to_string(),
                description: "General-purpose agent with full tool access.".to_string(),
                system_prompt: None,
                model: None,
                include_tools: Vec::new(),
                exclude_tools: Vec::new(),
                read_only: false,
                max_turns: None,
            },
        );

        agents.insert(
            "explore".to_string(),
            AgentDefinition {
                name: "explore".to_string(),
                description: "Fast read-only agent for searching and understanding code."
                    .to_string(),
                system_prompt: Some(
                    "You are a fast exploration agent. Focus on finding information \
                     quickly. Use Grep, Glob, and FileRead to answer questions about \
                     the codebase. Do not modify files."
                        .to_string(),
                ),
                model: None,
                include_tools: vec![
                    "FileRead".into(),
                    "Grep".into(),
                    "Glob".into(),
                    "Bash".into(),
                    "WebFetch".into(),
                ],
                exclude_tools: Vec::new(),
                read_only: true,
                max_turns: Some(20),
            },
        );

        agents.insert(
            "plan".to_string(),
            AgentDefinition {
                name: "plan".to_string(),
                description: "Planning agent that designs implementation strategies.".to_string(),
                system_prompt: Some(
                    "You are a software architect agent. Design implementation plans, \
                     identify critical files, and consider architectural trade-offs. \
                     Do not modify files directly."
                        .to_string(),
                ),
                model: None,
                include_tools: vec![
                    "FileRead".into(),
                    "Grep".into(),
                    "Glob".into(),
                    "Bash".into(),
                ],
                exclude_tools: Vec::new(),
                read_only: true,
                max_turns: Some(30),
            },
        );

        Self { agents }
    }

    /// Look up an agent definition by type name.
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.get(name)
    }

    /// Register a custom agent type.
    pub fn register(&mut self, definition: AgentDefinition) {
        self.agents.insert(definition.name.clone(), definition);
    }

    /// List all available agent types.
    pub fn list(&self) -> Vec<&AgentDefinition> {
        let mut agents: Vec<_> = self.agents.values().collect();
        agents.sort_by_key(|a| &a.name);
        agents
    }

    /// Load agent definitions from disk (`.agent/agents/` and `~/.config/agent-code/agents/`).
    /// Each `.md` file is parsed for YAML frontmatter with agent configuration.
    pub fn load_from_disk(&mut self, cwd: Option<&std::path::Path>) {
        // Project-level agents.
        if let Some(cwd) = cwd {
            let project_dir = cwd.join(".agent").join("agents");
            self.load_agents_from_dir(&project_dir);
        }

        // User-level agents.
        if let Some(config_dir) = dirs::config_dir() {
            let user_dir = config_dir.join("agent-code").join("agents");
            self.load_agents_from_dir(&user_dir);
        }
    }

    fn load_agents_from_dir(&mut self, dir: &std::path::Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && let Some(def) = parse_agent_file(&path)
            {
                self.agents.insert(def.name.clone(), def);
            }
        }
    }
}

/// Parse an agent definition from a markdown file with YAML frontmatter.
///
/// Expected format:
/// ```markdown
/// ---
/// name: my-agent
/// description: A specialized agent
/// model: gpt-4.1-mini
/// read_only: false
/// max_turns: 20
/// include_tools: [FileRead, Grep, Glob]
/// exclude_tools: [Bash]
/// ---
///
/// System prompt additions go here...
/// ```
fn parse_agent_file(path: &std::path::Path) -> Option<AgentDefinition> {
    let content = std::fs::read_to_string(path).ok()?;

    // Parse YAML frontmatter.
    if !content.starts_with("---") {
        return None;
    }
    let end = content[3..].find("---")?;
    let frontmatter = &content[3..3 + end];
    let body = content[3 + end + 3..].trim();

    let mut name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("custom")
        .to_string();
    let mut description = String::new();
    let mut model = None;
    let mut read_only = false;
    let mut max_turns = None;
    let mut include_tools = Vec::new();
    let mut exclude_tools = Vec::new();

    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = value.to_string(),
                "description" => description = value.to_string(),
                "model" => model = Some(value.to_string()),
                "read_only" => read_only = value == "true",
                "max_turns" => max_turns = value.parse().ok(),
                "include_tools" => {
                    include_tools = value
                        .trim_matches(|c| c == '[' || c == ']')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "exclude_tools" => {
                    exclude_tools = value
                        .trim_matches(|c| c == '[' || c == ']')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                _ => {}
            }
        }
    }

    let system_prompt = if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    };

    Some(AgentDefinition {
        name,
        description,
        system_prompt,
        model,
        include_tools,
        exclude_tools,
        read_only,
        max_turns,
    })
}
