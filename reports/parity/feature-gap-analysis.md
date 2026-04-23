# Feature Gap Analysis

This document tracks agent-code's feature coverage against the published behavior of the upstream reference TypeScript coding-agent CLI and a public Rust parity-port ("claw") for module-level breadth.

- **Target repo:** this one — agent-code
- **Upstream spec source:** the reference TS CLI's public docs only; no proprietary source or verbatim prose is reproduced
- **Reference-map source:** the public claw port's module list; used purely to surface "does this capability exist upstream?" — no code copied

## How to read this file

Each row is **(feature → upstream? → reference-map? → agent-code?)**:

- **Upstream** column says whether the feature is documented in the upstream public reference. `yes / no / partial`. When `no`, the row is either claw-internal scaffolding (drop) or a superset we already lead on.
- **Reference-map** column notes whether the claw port exposes a module. Use only to locate, not to justify.
- **agent-code** column cites an existing path (present) or `MISSING`.

Verified on main at commit `687017d`.

---

## Section 1 — Claw runtime modules cross-checked

| Module claw surfaces | In upstream? | In agent-code? | Notes |
|----------------------|-------------|----------------|-------|
| `bash_validation`    | yes (destructive-command detection) | present — `crates/lib/src/tools/bash.rs`, `bash_parse.rs` | Equivalent behavior, different file layout |
| `branch_lock`        | no — claw-internal | MISSING | Not a user-facing upstream feature; drop |
| `config_validate`    | partial — upstream validates schema on load | partial — `/doctor` validates hook configs (PR #227) | Could extend to permissions / MCP / sandbox |
| `green_contract`     | no — claw parity-harness internal | MISSING | Drop |
| `lane_events`        | no — claw parity-harness internal | MISSING | Drop |
| `mcp_client`, `mcp_server`, `mcp_stdio` | yes (MCP client + stdio transport) | present — `crates/lib/src/tools/mcp_proxy.rs`, `mcp_resources.rs`, `services/mcp/` | Coverage present, depth differs |
| `mcp_lifecycle_hardened` | yes — auto-reconnect with backoff | MISSING | Real gap. Agent-code MCP connections fail-once |
| `mcp_tool_bridge`    | yes (MCP → native tool bridging) | present — `crates/lib/src/tools/mcp_proxy.rs` | Equivalent |
| `oauth`              | yes — upstream supports OAuth-based login flow for managed auth | MISSING | Real gap, Anthropic-provider-specific. Users currently use API keys only |
| `permission_enforcer`| yes — per-tool/per-pattern permissioning | present — `crates/lib/src/permissions/mod.rs` (`PermissionRule`) | Equivalent (rule-based matcher, 5 modes) |
| `plugin_lifecycle`   | partial — upstream supports plugin install/reload | present — `crates/lib/src/plugins/` + `/reload` | Equivalent |
| `policy_engine`      | yes — policy evaluation for tool calls | partial — sandbox policy in `crates/lib/src/sandbox/`; permission rules in `permissions/`. No unified "policy engine" layer | Works in practice |
| `recovery_recipes`   | no — claw-internal | MISSING | Drop |
| `remote`             | yes — remote agent triggering (webhooks) | present — `crates/lib/src/schedule/` with webhook triggers, `RemoteTriggerTool` | Equivalent |
| `sandbox`            | partial — upstream has permission modes; OS-level sandbox is claw-specific | **we lead** — `crates/lib/src/sandbox/` with macOS seatbelt + Linux bwrap | Superset of upstream |
| `session_control`    | yes — session save/resume/export | present — `/resume`, `/session`, `/fork`, `/export`, `/transcript`, `/rewind`, `/redo`, `/rename` | Equivalent or ahead |
| `sse`                | yes — SSE transport for streaming | present — `crates/lib/src/services/bridge.rs` | Equivalent |
| `stale_base`, `stale_branch` | no — claw-internal git hygiene checks | MISSING | Drop; bash tool can surface via git status |
| `summary_compression`| yes — context compaction | present — `crates/lib/src/services/compact.rs` (microcompact + LLM compact + context collapse) | Equivalent or ahead |
| `task_packet`, `task_registry` | yes (todo list) | present — `TodoWriteTool` + `services::background::TaskManager` | Equivalent |
| `team_cron_registry` | partial — scheduled agent runs | present — `crates/lib/src/schedule/` with cron + webhooks + daemon | Equivalent |
| `trust_resolver`     | no — claw-internal | MISSING | Drop |
| `usage`              | yes — token/cost tracking | present — `crates/lib/src/llm/message.rs` (`Usage` struct, per-model breakdown) | Equivalent |
| `worker_boot`        | no — claw-internal orchestration | MISSING | Drop |

---

## Section 2 — Upstream features neither repo catalogs yet

Candidate areas from upstream's documented config/settings surface. Each is verified against agent-code source as of main.

| Feature | Upstream? | agent-code? | Priority | Notes |
|---------|-----------|-------------|----------|-------|
| `permission_mode: plan` | yes | present — `plan_mode` flag + `PermissionMode::Plan` | — | |
| `permission_mode: accept_edits` | yes | present — `PermissionMode::AcceptEdits` | — | |
| `permission_mode: bypass_permissions` | yes | partial — `disable_bypass_permissions` inverted setting + `--dangerously-skip-permissions` flag | low | Semantics match; key name differs |
| `allowed_tools` / `disallowed_tools` config keys | yes | present — `PermissionRule` supports per-tool + per-pattern allow/deny | — | Same capability, different shape (our rules are richer) |
| `apiKeyHelper` setting | yes | MISSING | **high** | Lets users run an external binary to fetch API keys dynamically (secret rotation, SSO flows) |
| `awsAuthRefresh` / `awsCredentialExport` | yes | MISSING | medium | AWS cred rotation for Bedrock users |
| `cleanupPeriodDays` setting | yes | MISSING | medium | Auto-prune session files older than N days |
| `enableAllProjectMcpServers` | yes | MISSING | low | Opt into all project-level MCP servers at once |
| Hierarchical `AGENTS.md` discovery (walk up parents) | yes | MISSING | **high** | Currently only root + user-global; should walk parents from cwd |
| `settings.local.toml` precedence (gitignored override) | yes | MISSING | **high** | Three-layer config today is user → project → CLI; need local override tier |
| Statusline customization | yes | MISSING | medium | Per-session status line with cwd / branch / cost / model |
| MCP reconnect with exponential backoff | yes | MISSING | **high** | A dead MCP server today stays dead for the session |
| OAuth login flow | yes | MISSING | low-medium | Only applies to the Anthropic provider path; API keys cover everyone else |
| Transcript export | yes | present — `/export` writes markdown, `/transcript` is the live viewer | — | |
| `/resume` + session forking | yes | present — `/resume`, `/fork`, `/session` | — | |

---

## Prioritized implementation list (Phase 2)

Ordered by user-impact. Every item is verified present in upstream and verified missing in agent-code.

### High priority

1. **Hierarchical `AGENTS.md` discovery** — walk from cwd up to repo root, loading each `AGENTS.md` / `CLAUDE.md`-compat along the way. Current implementation only checks repo root + user global. Fix in `crates/lib/src/memory/mod.rs`.
2. **`settings.local.toml` precedence** — load `.agent/settings.local.toml` as a fourth tier (gitignored, per-developer override). Fix in `crates/lib/src/config/loader.rs`.
3. **MCP reconnect with backoff** — exponential backoff + health check for MCP servers that die mid-session. Fix in `crates/lib/src/services/mcp/`.
4. **`apiKeyHelper` setting** — allow a config entry that names a binary; agent-code runs it and reads stdout for the key at session start (and on rotation). Fix in `crates/lib/src/config/schema.rs` + `ApiConfig::default`.

### Medium priority

5. **`cleanupPeriodDays` setting** — background prune of session files older than N days on agent startup. Fix in `crates/lib/src/services/session/`.
6. **`awsAuthRefresh` / `awsCredentialExport`** — shell-hook entry points for AWS cred rotation. Fix in the Bedrock provider path.
7. **Statusline customization** — user-configurable template evaluated against a rendering context. Separate module under `crates/cli/src/ui/`.
8. **Extended `config_validate`** — build on PR #227's hook validation to also cover MCP servers, permission rules, sandbox config shape.

### Low priority / skip

- `bypass_permissions` exact key-name match — semantics already covered.
- `enableAllProjectMcpServers` — niche, solvable by listing servers.
- OAuth login — only benefits a single provider, API-key path works everywhere.
- All claw-internal scaffolding (`branch_lock`, `stale_base/branch`, `recovery_recipes`, `green_contract`, `lane_events`, `task_packet`, `task_registry`, `trust_resolver`, `worker_boot`) — not in upstream; drop.

---

## Superiority axes (Phase 4 — post-parity)

Items where agent-code can exceed upstream by leveraging existing strengths:

1. **Multi-provider routing** — we have 15 LLM providers; add automatic per-task cost/latency routing and a `/best-model-for-this-task` skill. Upstream ships one provider.
2. **Real OS sandboxes** — macOS seatbelt + Linux bwrap are live; ship a `default_mode = sandbox-first` permission mode that upstream can't match without OS plumbing.
3. **Public nightly benchmark** — the existing `crates/eval/` framework can publish nightly SWE-bench-lite runs comparing agent-code, upstream, and Codex. Raw head-to-head numbers, not marketing claims.
4. **Visual diff preview for multi-file edits** — the Flutter desktop/web client can render diffs natively; upstream's terminal-only CLI can't.

A follow-up issue tracks these under "Superiority roadmap."

---

## Process notes

- The gap list was derived by grep-verifying each suspected claw module against agent-code's tree, then cross-checking against upstream public behavior. Internal-scaffolding matches were dropped.
- No source was read or copied from upstream. Feature descriptions paraphrase the public config/docs surface.
- No source was read or copied from the reference Rust port. Its `runtime/src/` module list was used as an index into "what might exist upstream," not as a reference implementation.
- Settings keys that mirror upstream use the same names so users can share configs — behavior is re-implemented independently.
