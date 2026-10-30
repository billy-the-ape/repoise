# AP-07: thin VS Code operations and benchmark UI

Status: proposed. Depends on [AP-01](../complete/agent-platform/01-workspace-and-ci.md) and
[AP-02](../complete/agent-platform/02-cli-contracts.md); benchmark UI also depends on AP-04/AP-05.
Home: `apps/vscode/`. May split into operations UI and benchmark UI PRs while
retaining this boundary. No marketplace identity or publication is authorized.

## Outcome and architectural boundary

Provide a simple way to inspect setup/freshness/coverage, run indexing, manage
watch, configure MCP and launch/view benchmarks. Call the installed Repoise CLI;
do not implement indexing, retrieval, agent loops, evaluation or statistics in
editor code. Every benchmark remains runnable without VS Code. Editor overhead
does not enter CLI benchmark results unless explicitly measuring that surface.

Discover an explicit user override, repository-local package installation and
then PATH in documented precedence. Prefer the local package when unambiguous.
Resolve the actual binary in the relevant extension host without an implicit
package-manager download. Probe version/capabilities and show incompatibility or
missing-install actions. Never assume a native archive user has Node available
for Repoise itself; extension runtime and optional benchmark dependencies are
separate requirements.

Use argument arrays, explicit root/cwd and AP-02 events. Render progress, phase
percentage/estimated ETA when available, embedding pending/failed counts and
actionable errors. The UI must not guess percentage from log lines. Keep secrets
in supported credential mechanisms/references and out of output channels/webviews.

## Workspace scope and watch ownership

Model each workspace folder/root/worktree separately, including nested repositories
and multi-root windows. Show selected scope before actions; do not bind MCP/index
to whichever directory happened to be active at extension activation. Detect
branch switches and root removal; unsaved editor buffers are outside Repoise's
filesystem snapshot. Preserve expected-hash stale-read safeguards even if watch
looks healthy.

| Situation                               | Expected behavior                                                               |
| --------------------------------------- | ------------------------------------------------------------------------------- |
| User-managed watch already exists       | Display/attach to supported status; do not silently replace or kill it          |
| Extension owns a watch                  | Supervise lifecycle, report health, cancel on owned-root disposal as documented |
| Two windows target one worktree         | Detect ownership; avoid duplicate competing writers and restart loops           |
| Separate worktrees share content caches | Keep each mutable index scope/root distinct                                     |
| Crash or stale ownership marker         | Verify identity/health; PID reuse is not proof of ownership                     |
| Provider unavailable/partial vectors    | Show lexical freshness separately from vector coverage and last error           |

Use AP-02 ownership capabilities; do not invent a second lock protocol. Reuse
saved-files watch behavior rather than indexing on every unsaved keystroke. Avoid
continuous status polling or index rebuilds merely because a panel opens. Define
bounded restart/backoff and manual recovery; the extension is not a new daemon.

For Remote SSH, WSL and containers, resolve binaries/paths and run processes where
the target filesystem lives. A local Mac path must not be passed to a remote
Linux agent. Record supported host/remote combinations and executable platform
requirements. Workspace Trust gates execution of repository-local binaries and
configured tasks; opening an untrusted checkout must not launch arbitrary code.

## MCP integration

A stdio MCP client normally launches its own server. Provide a definition using
the installed binary and explicit selected root/snapshot; do not launch a detached
server and assume clients connect to it. Read-only stays default; refresh remains
explicitly enabled. Multiple client-owned read servers may coexist with watch.

VS Code offers `vscode.lm.registerMcpServerDefinitionProvider` for editor MCP
discovery. Verify the targeted VS Code version/API before implementation. Cline,
Codex and other extensions may use their own MCP settings/lifecycle. Maintain a
tested client/version matrix; do not claim registering in VS Code configures
every installed agent. Where integration is unsupported, provide scoped setup
instructions or an explicitly chosen config change, preserving unrelated entries.

Root changes/worktree switches must produce updated definitions; a client may
need to restart its server. Never silently continue showing a server for the old
checkout as healthy for the new one. Surface permissions, enabled refresh and
effective embedding configuration without exposing credential values.

## Benchmark interface

Select suite, repository snapshot, adapter/harness, model profile, retrieval
conditions, repetitions and budgets. Show dry-run trial count and prerequisites.
Launch the CLI experiment and display its streaming progress, cancellation and
artifact locations. Do not run tests automatically on install/window activation.

Render reports produced by the common reporting layer: correctness, timing,
input/output/cache usage, uncertainty, errors/degradation, unavailable metrics
and per-task/repository comparisons. Preserve failures; no green result based
only on agent exit code. A generic smoke suite should be labeled as such.

Persist experiment state on disk through the CLI contract. Panel closure/window
reload must not lose results. Define whether an editor-owned experiment is
cancelled on shutdown or can be reattached; initial default should be explicit
owned-process cancellation with preserved partial records unless AP-04 supports
durable execution. Do not promise resumability by spawning an orphan process.

Treat transcripts and HTML/Markdown reports as untrusted content. Render escaped
text, restrict webview resources/commands and prevent arbitrary report scripts.
Do not auto-upload benchmark artifacts or private repository content. UI telemetry
must not become an implicit benchmark data collection service.

## Scope, tests and release

First slice: binary/capability discovery, root selection, status/index/progress,
watch ownership and explicit MCP definition/setup. Second slice when contracts
land: benchmark configuration, execution/cancel and common report display. Avoid
an unrelated settings editor, model manager or agent implementation.

Use fake CLI event streams for UI tests; test missing/old executable, paths with
spaces, Windows cancellation, duplicate windows, multi-root, root changes,
untrusted workspace, remote-host path resolution, malformed output, partial
vectors and interrupted runs. Add installed-CLI contract smoke tests and packaged
VSIX inspection. Agent integration claims require a live client/version smoke,
not only an API mock. Offline UI tests need no inference credentials.

Define extension ID/publisher, minimum VS Code engine, supported OS/remote matrix,
VSIX allowlist, license notices and independent release/version compatibility.
Development builds must not bundle the whole monorepo, private fixtures or a
silently downloaded binary. Whether to bundle any native binary is deferred;
initial direction is discovery of the package-manager/native installation.
Publication and registry identity setup are separate owner-authorized actions.

## Open questions and references

Resolve supported agent clients/settings APIs, executable discovery under each
package manager, cross-window ownership/status transport, remote extension kind,
workspace trust prompts, reload behavior and compatible release ranges. A future
service/daemon is a separate measured design decision, not a prerequisite.

Primary reference checked October 19, 2026:
[VS Code MCP developer guide](https://code.visualstudio.com/api/extension-guides/ai/mcp).
Revalidate API availability and each agent's actual integration when implementing.
