# VS Code extension (AP-07 first slice)

Current state: the `apps/vscode/` package is a thin VS Code operations UI over
the installed Repoise CLI. It is a development build inside this repository;
there is no marketplace identity, no publication, and no bundled native
binary. The second slice (benchmark configuration/execution/report display)
is proposed work — see the
[AP-07 plan](plans/agent-platform/07-vscode-extension.md).

The extension never implements indexing, search, watch logic, retrieval or
agent behavior. Every operation shells out to the native CLI with argument
arrays and renders the AP-02 machine contracts
([automation contracts](automation.md), [MCP server](mcp.md)).

## What it does

- **CLI discovery** (`repoise.cliPath` override → repository-local
  `node_modules/.bin/repoise` per workspace folder → PATH; on Windows both
  `repoise.cmd` and `repoise.exe` are probed, so npm installs, native-archive
  installs and `cargo install` all resolve), then a `capabilities --json`
  handshake that reports version, compatibility problems and the source that
  was selected. A missing or broken binary is a panel error, never an implicit
  download. A failed discovery is re-probed on the next action (refresh,
  index, watch start, MCP), so installing the CLI or fixing PATH recovers
  without a window reload.
- **Status panel** (one webview; `repoise.showStatus`): one entry per
  workspace folder/root parsed from `status --json` (exit 0 or 3 are
  parseable; exit 3 = index missing/stale). Shows scope, snapshot, index
  generation/coverage, freshness, writer/owner state and last error. A single
  status refresh happens when the panel opens; it does not poll.
- **Index** (`repoise.index`): runs `index --events` for the selected root and
  renders `progress` events (phase, done/total, elapsed, ETA when the CLI
  supplies one), `warning` events and the terminal outcome. It does not guess
  percentages from log lines.
- **Watch supervisor** (`repoise.watchStart` / `repoise.watchStop`): one
  extension-owned `watch --events` process per selected root. External
  owners are detected from `failed … reason: scope-owned` (the extension
  hands off and never restarts into a restart loop), bounded restart/backoff
  for crashes, cooperative SIGTERM cancellation, and cancellation when a
  workspace folder is removed or the window closes. A Start issued while a
  Stop is still draining (the cancelled process has not settled) is
  reported, not silently dropped. The panel's Start/Stop buttons follow
  the supervisor status, so Stop is also available during the startup
  probe and a restart backoff, and an in-flight Start is reported
  instead of silently ignored.
- **MCP** (`repoise.mcpShow`): a read-only `mcp` definition per workspace
  folder (`refresh` only when `repoise.mcp.refresh` is true, matching the
  CLI's opt-in), plus VS Code's
  `vscode.lm.registerMcpServerDefinitionProvider` registration on hosts that
  provide it (1.103+). Registering with VS Code does not configure every
  installed agent; the panel also offers a generic stdio `mcpServers` JSON
  snippet for Cline/Codex-style clients. The extension never writes to
  client settings files. The provider fires
  `onDidChangeMcpServerDefinitions` when the workspace is trusted, when a
  `repoise` setting changes, or when workspace folders are added or
  removed, so VS Code re-queries the definitions (binary,
  `--allow-refresh`, folder list) without a window reload.
- **Trust gating**: repository-local binaries only run in a trusted
  workspace. The extension activates in Restricted Mode with
  `untrustedWorkspaces.supported: limited` and `repoise.cliPath` as a
  restricted setting; in an untrusted checkout discovery falls back to PATH
  only (no override path, no per-folder `node_modules` probe), commands
  prompt for trust, and auto-watch never starts — no repository-local code
  is launched.
- **Windows launch safety**: when the selected binary is an npm `.cmd`/`.bat`
  shim, it is invoked through `cmd.exe /d /s /c` with a single payload in
  which every token (shim included) is double-quoted and the whole payload
  carries one outer quote pair that `/s` strips, so metacharacters in paths
  are literal; the spawn uses `windowsVerbatimArguments`. Arguments
  containing `"`, `%` or line breaks are rejected with a panel error
  instead of being spliced into a command line.

## Settings

| Setting                  | Default | Meaning                                                        |
| ------------------------ | ------- | -------------------------------------------------------------- |
| `repoise.cliPath`        | `""`    | Explicit binary override; a missing path is an error, not a fallback |
| `repoise.watch.autoStart`| `false` | Start an owned watch for the selected root when the panel opens or the root selection changes; fires at most once per root per panel session and never after an explicit Stop, exhausted restarts or an external owner |
| `repoise.mcp.refresh`    | `false` | Include `--allow-refresh` in MCP definitions                  |

## Development and tests

```sh
pnpm install
pnpm --filter @repoise/vscode run test
```

The suite compiles TypeScript, then runs `node --test` against a
deterministic fake CLI fixture (`src/test/fixtures/fake-repoise.mjs`) that
emits the AP-02 event envelope: happy streams, malformed lines, unknown event
types, unsupported schema versions, scope-owned failures, crashes, CRLF
line endings, cooperative SIGTERM cancellation and timeouts. No inference
credentials or installed CLI are required; the fake's signal handlers are
armed before first output because a parent that kills the child in the same
instant it observes the first line must never race handler setup. CI runs the
suite in all three Check jobs (Linux/macOS/Windows) via `turbo run test`;
Windows cancellation asserts the documented abrupt `TerminateProcess`
semantics (via a whole-process-tree `taskkill /PID <pid> /T /F` when the npm
cmd shim is involved, so the node launcher and native descendants cannot
survive holding the scope lock and inherited pipes) rather than a terminal
event.

Verified against the installed CLI: `capabilities --json`, `status --json`
(snake_case view, top-level owner marker), `index --events` and
`watch --events` with SIGTERM cancellation all parse through the extension's
stream/status parsers (checked November 3, 2026).

## Deferred / not claimed

- Benchmark configuration, execution, cancellation and report display
  (AP-07 second slice; depends on the AP-04/AP-05 contracts).
- VSIX packaging, extension publisher/identity, remote-host matrix
  (Remote SSH/WSL) and a live client/version MCP compatibility matrix.
- Cross-window ownership transport beyond the OS scope lock and the status
  owner marker; a service/daemon remains a separate decision.
