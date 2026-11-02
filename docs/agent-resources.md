# Local Skills and MCP

Open **Agent Configuration → Skills** or **MCP**. Management is limited to the
current desktop user's home directory:

| Client | Skills directory | MCP configuration | Transports |
| --- | --- | --- | --- |
| Claude Code | `~/.claude/skills` | `~/.claude.json` | stdio, HTTP, SSE |
| Codex | `~/.codex/skills` | `~/.codex/config.toml` | stdio, HTTP |
| Gemini CLI | `~/.gemini/skills` | `~/.gemini/settings.json` | stdio, HTTP, SSE |

Custom environment directories, project configurations, shared `.agents/skills`
directories, remote catalogs and other clients are not managed in this version.
Restart or reload clients after changes. Installed Skills are enabled; disabling
a managed Skill removes its target copy rather than changing a client setting.

## Safety

Import a local directory containing `SKILL.md`, choose an identifier and select
targets. Import creates an independent source copy under
`~/.config/easy-cli-proxy-api/agent-resources/skills`. The original source and
unmanaged installations are never removed. Conflicting names, symbolic links,
path traversal, and externally modified managed copies are rejected. To update
Skill contents, import the updated source under a new identifier.

Import/create and edit/remove are explicit operations. Importing a managed Skill
identifier or creating an existing MCP identifier is rejected before any writes,
including when an MCP entry is disabled. Use Manage to edit an existing resource;
Skill edits retain the cached source and its fingerprint protection.
Empty directory layout is preserved and included in the modification guard;
existing file-only fingerprints remain compatible. MCP conflict checks and
proposed writes use the same configuration and registry reads.

The MCP editor masks environment variables and headers by default. It does not
execute commands or start servers. Clients may start enabled servers when they
next load their configurations. Disabling removes a server from the selected
client's active configuration and stores its draft in the private resource
registry. Other servers and unrelated settings are preserved, including unrelated
TOML comments. JSON formatting may change. Writes use private temporary files,
atomic replacement, `.ecpa-backup` files and rollback on write failures.
Malformed configurations block saving rather than being replaced. Refresh and
reopen an editor after an external configuration change.

## Fixture validation

Run these commands sequentially, not concurrently:

```sh
bun run check
bun run build
cargo test --manifest-path src-tauri/Cargo.toml --bin cpa-gui agent_resources::tests
bun test tests/agentResources.test.ts tests/i18n.test.ts tests/navigationAndUpdate.test.ts tests/cssThemeLint.test.ts --timeout 30000
bun run test:ui:agent-resources
```

The UI script requires Playwright (or an absolute `PLAYWRIGHT_MODULE` module path)
and an installed Chromium browser. It starts its own loopback Vite server with an
isolated temporary cache, uses browser fixtures, and closes only its own browser
and server. Backend tests use temporary home fixtures, never real client settings.
Screenshots of Skills, MCP and the masked editor are written to
`/tmp/ecpa_issue-490-skills-mcp-1102_ui_*.png`.

The global CSS token lint currently reports existing violations in
`src/components/routing/RoutingPanel.css` and `src/styles.css`. Keep that check
enabled; validate the new panel independently with:

```sh
bun test tests/cssThemeLint.test.ts --test-name-pattern 'keeps resource management styles' --timeout 30000
```

The broader theme background contract also reports existing literal colors in
`src/pages/AgentUsageView.css`; these unrelated styles are not changed here.
