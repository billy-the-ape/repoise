# Changelog

## Unreleased

### Code chunk bounds across all declaration forms

TS/JS ordinary function declarations used size-aware splitting, but arrow
functions, function expressions, variable-bound classes, methods, nested
functions and generic declarations were emitted through `emit_unit`
regardless of size, and a single oversized statement or block item could be
retained whole. A routes-table arrow function like the pilot's
`chatRoutes` therefore produced a chunk far beyond the embedding model
context and had to be excluded per repository.

- One size-aware emission/splitting path now covers every declaration form:
  statements/body items group into bounded spans (function and class
  splitting), single oversized items split by line windows, and the
  line-window fallback for non-grammar code uses the same strict packing.
- The 1,000-estimated-token ceiling (characters ÷ 4, an estimate — not a
  promise about any provider's tokenization or context window) now holds on
  every splittable emitted chunk, counting the inter-line newlines that the
  older per-line packing ignored.
- Irreducible input — a single source line already larger than the budget —
  is a documented exception: the line is emitted whole, exactly (exact
  line/byte ranges, full text, searchable and exact-readable) and labeled
  via the parse result's `oversized` flag; it is never truncated, dropped or
  silently relabeled. Blank lines and pure boundary-punctuation lines (e.g.
  a lone closing brace) remain intentionally outside every unit's range, as
  before.
- Parser/artifact fingerprints bump to `ts/tree-sitter-0.23.2/2`,
  `js/tree-sitter-0.25.0/2` and `code-line/2`.

**Compatibility note:** code files reparse once on the next index (chunk ids
bind the parser version), and their embedding inputs change, so
profile-keyed embedding cache entries for affected chunks regenerate
automatically. No destructive storage migration: older shared parse
artifacts (older artifact format version or parser rule version) are safe
misses that force a reparse and are overwritten by the refreshed artifact;
no config, environment or deployment action is required.

### Authoritative one-writer-per-scope ownership lock

`OwnerGuard` previously published a best-effort advisory `owner.json`
marker: a second writer warned and proceeded, so two windows could create
competing writers on the same mutable index scope. Writers now acquire an
authoritative, cross-platform OS-backed lock
(`crates/repoise-core/src/ownership.rs`; `flock(2)` on Unix, `LockFileEx` on
Windows, via pinned `fs2 =0.4.3`) on
`<cache>/repos/<repoId>/worktrees/<worktreeId>/scope.lock` **before any
provider work or publication**:

- `index`, `watch` and the opt-in MCP `refresh_project_knowledge` all run
  through the same lock. Acquisition is atomic across processes and is
  released by the OS when the owning process dies (clean exit, crash or
  kill) — never dependent on PID freshness or unsafe stale-file deletion.
- A second writer for the same scope is refused with a stable, actionable
  busy result before any provider work or publication: exit code 1 with the
  message on stderr for one-shot operations, and a `failed` terminal event
  carrying the machine `reason` in the `--events` stream — `scope-owned`
  with `retryable: true`, or `lock-unavailable` with `retryable: null` when
  the lock file itself could not be used. Existing exit-code meanings are
  unchanged; the refusal is an operational failure.
- The `owner.json` marker remains as descriptive heartbeat metadata for the
  process holding the lock (pid, operation, start/refresh times, atomic
  temp+rename publication); a PID is never proof of liveness and no process
  is ever killed or adopted based on it. Orphaned markers from dead writers
  are reported and replaced on the next successful acquisition.
- `status` gains an additive `writer` field (`lock_held`, `state`: `owned` |
  `busy` | `stale-metadata` | `free`) alongside the existing descriptive
  `owner` field; `capabilities --json` advertises the new
  `exclusive-scope-lock` feature.
- `purge` acquires each scope's lock before removing it, so an active
  writer's scope can never be removed (or bypassed) from under it: a scoped
  purge of an owned scope fails before removing anything, and `purge --all`
  skips owned scopes, reporting them in the additive `skippedOwned` report
  field (human output prints a `skipped (owned):` line per scope).
- Read-only operations (`status`, `search`, `read`, `related`, `check`,
  read-only MCP tools) keep working while a writer owns the scope; different
  worktree/snapshot scopes never block each other.

**Compatibility note:** upgrading while an older (pre-lock) advisory
`owner.json` exists is safe — the marker is descriptive only; the lock is
new and starts free. On supported local filesystems a crash never needs
manual recovery: the OS releases the lock, and a leftover marker is
reported as `stale-metadata` and replaced on the next acquisition.
Multi-client network filesystems (e.g. NFS) are not supported for shared
scope locking (documented in
[docs/automation.md](docs/automation.md#writer-ownership)).

### Live configuration reload for running watch

A running `repoise watch` previously loaded the effective configuration and
the embedding client once at startup: editing the configuration during a
watch run had no effect until the watch was restarted. The watch now
reconciles the resolved configuration sources on every step (the explicit
CLI-selected paths, otherwise the committed `repoise.config.json` and the
local override — monitored even when excluded from the corpus) and applies a
changed effective configuration as one config-driven generation on the same
process, without mixing old and new settings within a publication:

- Include/exclude patterns, role rules, preset, max file size, and the
  embedding model/profile/input template/scope are reloadable; unchanged
  effective settings never trigger a rebuild. A model or input-template
  change rebuilds the embedding client (when the watch can build one) and
  never reuses vectors from the old profile. The effective-config
  fingerprint now includes `embedding.endpoint`, so an endpoint-only change
  also applies as a config-driven rebuild instead of being silently
  ignored. **Upgrade note:** the fingerprint format change means every
  existing embedding-configured index reports stale after upgrading and
  needs one reindex (`repoise index`, or the next watch publication);
  vectors are still reused through the content-addressed cache, so
  unchanged inputs cost no provider calls; lexical-only indexes are
  unaffected.
- An invalid configuration freezes new publications: repeated identical
  warnings are throttled, and the last published generation stays served.
  Warnings are sanitized — they name the structurally broken file when
  identifiable and report only the validation problem count; user-typed
  values and credentials are never included (details via `repoise doctor`).
  Fixing the file recovers automatically in the next generation.
- Process-bound settings (`cacheDir`, `sharedCache`, `history`) are never
  applied in process: the warning explicitly says a restart is required and
  no new generations are published until the setting is reverted or the
  watch is restarted (the watch keeps its startup settings in the meantime).
  The no-factory client path tracks
  the applied embedding revision when it drops the client for a lexical-only
  configuration, so a later revert to embedding settings reports
  restart-required instead of publishing a lexical-only generation that
  carries the embedding fingerprint.
- `watch --events` warning payloads gain an additive `kind` field
  (`"invalid"` / `"restart-required"`), documented in the event-stream
  schema and the [automation guide](docs/automation.md); watch
  `progress` publication events already carry `configChange`.

### Freshness separated from Git dirtiness

`status` previously reported the working-tree index as stale whenever Git
reported any uncommitted changes, even when the published manifest and
configuration exactly matched the saved working tree — so a coding-session
UI could stay stale after a successful `watch` or `index` publication.
Freshness now means the index represents the selected filesystem snapshot:
it is judged from the published content manifest and effective-config
fingerprint for the declared scope/mode (parser compatibility is enforced
at publication by `indexing`, as before). Git dirtiness is a separate,
informational concern: `snapshot.dirty_count` and `freshness.dirty_count`
(and the human `dirty:` line) still report it, and branch/revision
information is unchanged, but uncommitted changes never make the index
stale.

Consequences:

- An edited, saved, tracked file that is indexed (manually or via `watch`)
  is **fresh while `dirty_count` is non-zero**. Unsaved editor buffers
  were always outside the filesystem snapshot; saved uncommitted content
  can now be indexed and fresh.
- Stale still means: eligible saved content changed after publication, the
  effective configuration changed, the scope/mode no longer matches, or the
  index is absent (`unknown`). Committed-mode snapshots are unaffected by
  working-tree edits; branch labels alone never make an index stale — only
  changed snapshot content does.
- Exact reads keep their hash validation: a source id from a generation the
  live snapshot has moved away from still fails with a stale-source error,
  regardless of what `status` says.
- `check --fresh`, the MCP `project_knowledge_status` tool, the CLI and the
  benchmark preflight all use the same freshness judgment. The benchmark's
  clean-committed-snapshot preflight rule is independent of index freshness
  and still rejects a dirty consumer checkout.
- Response shapes and exit codes are unchanged: `status` still exits 3 when
  the index is missing or stale, and `check` keeps `ok`/`missing`/`stale`/
  `coverage-not-met` with exit codes 0/3/4.

### Scope-aware embedding coverage

Vector coverage is now computed against the eligible corpus of the
generation's persisted embedding scope (docs, or docs + code when the scope
is `all`) instead of the whole chunk population. Indexing embeds docs, or
docs + code, while config and other corpora stay lexical-only; previously
`status` computed `vectors_pending` as all chunks minus vector count, so a
healthy docs-only (or docs+code) index with config chunks looked
permanently incomplete and the bench hybrid preflight was unreachable for
such corpora. One shared computation now feeds `status`, `check`, the
search coverage report and the bench root hybrid gate:

- `status` (CLI and MCP, same view) gains additive machine fields on the
  index: `embedding_scope`, `eligible_chunks`, `ineligible_chunks` and
  `embedding_coverage` (`disabled`, `profile-unavailable`, `empty`,
  `partial` or `complete`); `vectors_pending` now counts eligible chunks
  without a vector for the stored profile (0 for lexical-only builds and
  empty eligible corpora). The human `status` line reports
  `N / M eligible (scope S; K lexical-only)` instead of `N / M chunks`.
- Vectors from a different profile fingerprint never satisfy coverage, and
  the judgment always uses the generation's persisted scope/profile, never
  today's config: changing the embedding scope/profile flags the index
  stale (rebuild required) instead of reinterpreting old vectors.
- `check` keeps its categories and exit codes: a configured embedding over a
  lexical-only served generation reports `profile-unavailable` (rebuild,
  not pending work), a stored profile over an empty eligible corpus is a
  deterministic unmet coverage with nothing pending (hybrid retrieval
  cannot run), and partial eligible coverage fails with the pending count.
- The bench hybrid preflight accepts any otherwise valid corpus whose
  eligible chunks are fully embedded (no corpus role remapping needed) and
  still rejects partial/missing coverage; `bench --dry-run` now also reports
  `eligibleChunks` and `embeddingCoverage`.
- No database migration: the `generation` row already stores
  `vector_profile` and `embedding_scope`; retrieval (RRF, ranking, query
  profiles) is unchanged. See the vector coverage semantics in
  `docs/storage.md`.

### Policy-aware, bounded intake

Filesystem and Git intake no longer read or hash corpus content before
policy applies. Adapters expose a metadata-only intake (native ignore files
plus candidate path/size pairs): the filesystem walk collects metadata and
`.gitignore` policy files without reading file contents, and committed Git
snapshots use `git ls-tree -r -z -l` plus `.gitignore` blob reads only.
Discovery then decides each candidate with the shared policy and reads
accepted, size-eligible files exactly once, bounded at `maxFileBytes + 1`,
hashing the accepted bytes once and reusing them for the manifest. Excluded
or oversized files (dependencies, build output, `.git`, generated caches)
contribute zero content reads or hashes; a file that grows after the
metadata check is bounded and reported (`file_too_large`), and a size drift
within the bound is still reported as `changed_during_scan`. Traversal
conservatively continues below excluded directories so nested `.gitignore`
policy files remain visible to the policy (no corpus content below them is
ever read), and the merged scan race behavior is preserved: a missing root
is fatal, entries that vanish mid-walk are tolerated, and genuine
persistent failures on eligible files remain errors. `index`, `status` and
`watch` share the same inventory path as before; manifests, hashes, skip
diagnostics and scope isolation are unchanged for unchanged trees.
`inheritGitIgnores: false` is honored by the inventory exactly as before
(native ignore files are still collected but contribute no policy rules,
matching `explain`/`doctor`).

### Workspace and CI foundation (AP-01)

The development tooling became a Cargo + pnpm workspace. pnpm 12.3.4 is pinned by the root `packageManager` field; the committed `pnpm-lock.yaml` replaces `package-lock.json` for development installs (`pnpm install --frozen-lockfile --ignore-scripts`). Turborepo 2.11.9 declares the root development tasks (`format`, `format:check`, `test`) and single per-platform native bridge tasks (`check:native`, `build:native` and their `:windows` variants) with output caching disabled; Cargo remains authoritative for Rust and keeps working without Node. The published launcher stays outside the pnpm workspace, so root frozen installs never resolve its optional platform-package pins; CI jobs provision the pinned pnpm with lifecycle scripts enabled (pnpm 12's postinstall swaps in the native binary) outside the checkout, with an in-workflow version guard.

CI now selects affected check sets for pull requests through `scripts/ci/impact-map.json` and `scripts/ci/select-checks.mjs` (unknown paths, empty diffs and selection failures run the full suite), and a stable aggregate `CI gate` job enforces expected results, failures and cancellations. Pushes to main always run the full suite, and the required `Check (OS)` job names are unchanged.

Release tooling resolves the real npm CLI even when pnpm points `npm_execpath` at pnpm's own CLI. Registry packing/staging semantics, the published packages and consumer installation are unchanged; no consumer migration is required. See `docs/plans/complete/agent-platform/01-workspace-and-ci.md` for the implementation record.

### CLI automation contracts (AP-02)

The CLI gained machine-automation surfaces without changing existing
`--json` output:

- `repoise capabilities` (and `--json`) — version/capabilities handshake:
  event-stream protocol version and checked-in schema, build features,
  command/machine-output matrix, benchmark availability (`bench`, shipped in
  this cycle alongside AP-04), the stable exit-code table (0 ok, 1
  operational, 2 usage, 3 index missing/stale, 4 coverage not met) and the
  additive compatibility policy. Schema: `schemas/capabilities.v1.schema.json`.
- `repoise index --events` and `repoise watch --events` — NDJSON event
  streams on stdout (started, phase-scoped progress, warning, 15 s
  heartbeat overridable via `REPOISE_EVENTS_HEARTBEAT_MS`, and exactly one
  terminal of completed/cancelled/failed on controlled exit); diagnostics
  stay on stderr. Envelope: `schemaVersion`, `runId`, strictly increasing
  `sequence`, UTC ISO-8601 `timestamp`, `type`, typed `payload`.
  Schema: `schemas/event-stream.v1.schema.json`. A stream without a
  terminal event is incomplete output.
- `watch` now cancels gracefully on SIGINT/SIGTERM (Unix; `Ctrl+C` on
  Windows) via `ctrlc` with the `termination` feature: the last complete
  generation is retained, a `cancelled` event is emitted and the process
  exits 0. `index` remains abruptly interruptible (transactional
  publication preserves the last complete generation).
- Advisory writer-ownership marker (`owner.json` in the worktree scope
  directory): `index` and `watch` acquire/refresh/release it without
  blocking; a fresh foreign marker warns on stderr and the run proceeds
  (publications stay transactional); `status` reports it read-only
  (`owner` field, `active`/`stale` state; 10-minute staleness window,
  5-second refresh); `purge` removes it with the scope.
- Watch publication events/reporting now include `configChange`
  (effective-configuration-driven publication; false on the first build).
- Reliability follow-ups from PR review: `index --events` session-setup
  failures emit a `failed` terminal event (the stream always ends with one
  terminal on controlled exit); the filesystem scan skips entries that
  vanish mid-scan so a `status` probe no longer fails while `watch`
  publishes into the in-root cache; on Windows the scan also tolerates
  delete-pending `PermissionDenied` via a metadata re-check; owner marker
  writes are atomic (sibling temp file + rename) so concurrent readers never
  observe a partial record; the scan's vanished-entry tolerance no longer
  applies to the scan root itself (a vanished root fails rather than
  publishing an empty inventory); `--events` streams always emit `started`
  before the first heartbeat; `Store::open` sets the busy timeout
  before the WAL pragma so concurrent first opens of a new database wait
  instead of failing.

No new environment variables are required (`REPOISE_EVENTS_HEARTBEAT_MS`
is an optional override); no migration or deployment actions. See
[docs/automation.md](docs/automation.md) and
`docs/plans/complete/agent-platform/02-cli-contracts.md` for the implementation
record.

### Agent adapter protocol and first adapter (AP-03)

A versioned external-agent adapter protocol landed as two pnpm workspace
packages: `agent-adapter-protocol` (request/event JSON schemas under
`schemas/`, and a protocol host that spawns the adapter in its own
process group, enforces the hello handshake, budgets and protocol rules,
and always returns exactly one result — a `final` event is present only
when the adapter emitted a valid one) and `agent-adapter-cline` (the first
adapter, driving the installed Cline headless CLI with per-trial isolated
state, per-trial MCP settings, an immutable redacted effective-config
artifact, and credential passing only through a spawn-time `-k` flag).
The Cline adapter is not an npm package. Host enforcement includes the
hello timeout, per-line byte and event-count limits, strict envelope
validation, cumulative/delta usage semantics, wall-clock escalation
(SIGINT → SIGKILL, and a cooperative `cancelled` final never replaces a
host-initiated timeout), post-final reading (events after the final are
rejected as `protocol_error`), a best-effort process-group sweep after
every path (timers, abort listeners and the sweep are guaranteed by a
`finally` block; a throwing `onEvent` callback is recorded as a warning
instead of aborting the run), blank first lines never disarm the hello
timeout, and credential scrubbing from all forwarded Cline output
(redacted before any truncation, so a key straddling a truncation boundary
cannot leak). The Cline adapter resolves executables into a launch spec:
explicit `.js`/`.mjs`/`.cjs` paths run through `process.execPath` (Windows
cannot execute shebang scripts), with bare `cline` documented as requiring
an explicit script path or native executable on Windows. Cancellation is
cooperative and cross-platform: the host keeps the request stdin open and
sends a `cancel` control line on cancellation (the only graceful channel on
Windows, where signals cannot reach console-less Node processes; POSIX
adapters additionally receive a group SIGINT), escalating to a forced kill
after the grace window; the Cline child runs in its own process group on
POSIX so the child's `aborted` result can no longer race the adapter's own
handler, and the adapter sweeps that child group (SIGKILL) on every final,
on exit, and when its own stdout closes (host death) with a 3s stop
escalation shorter than the host's default 5s grace so a host forced-kill
cannot orphan it; the adapter also records the child's root pid in
`child-pid.txt` under the artifact dir, and the host sweeps that recorded
pid after every path - the backstop that covers protocol-violation kills,
which SIGKILL the adapter before any of its own cleanup can run; the
host clears a stale `child-pid.txt` at run start and only sweeps pids
above 1 that are neither the host nor its parent; a `cancel` control line in
the same stdin chunk as the request is never discarded, and a stop that
lands before the spawn suppresses the spawn. The
checked-in schemas were corrected to match the implemented contract (the
request description now says stdin stays open for `cancel` control lines;
the hello usage-semantics object is scoped to `t: hello`; the final usage
carries the normalized totals shape with an optional `estimated` flag;
`profile.sampling` accepts null), and both suites now validate their own
requests and event streams against the schemas with ajv so schema/validator
drift fails CI. Coverage: 23
protocol conformance tests and 13 adapter tests (stub-CLI driven through
the real host), wired into CI through per-package `test` scripts run by
`turbo run test` in the Check jobs. No environment variables
are required for the protocol host itself; the Cline adapter honors
`CLINE_EXECUTABLE` and the documented `CLONE_*` passthrough namespace. See
[docs/agent-adapters.md](docs/agent-adapters.md) and
`docs/plans/complete/agent-platform/03-agent-adapters.md` for the implementation
record.

### Benchmark runner (AP-04)

`repoise bench` runs suite-driven agent benchmark experiments: repeated
trials over one frozen Git snapshot in disposable linked worktrees, against
declared retrieval conditions (`baseline`, `repoise` with enforced
`lexical`/`hybrid` through a read-only `repoise mcp` server bound to each
trial worktree, pinned to the declared `searchMode` with the new
`repoise mcp --search-mode` flag). The coordinator owns scheduling, isolation, budgets, interruption
and artifacts; it never implements an agent loop — every trial's agent
process is driven through the AP-03 protocol via the checked-in host bridge
(`packages/agent-adapter-protocol/bin/adapter-host-run.mjs`), with the
coordinator sending the cooperative `cancel` line on SIGINT/SIGTERM,
escalating after a grace window and force-killing the bridge and its
recorded process tree at the outer wall-clock deadline (exit 4, remaining
trials `not_started`). Preflight (exit 3) freezes the experiment: the
strictly parsed suite manifest (schema v1,
`schemas/bench-suite.v1.schema.json`; suite/prompt files must live outside
the benchmarked repository), a reproducible snapshot (clean commit or
recorded patch; the resolved Repoise cache directory never counts as dirt),
node/bridge/adapter resolution, `env:`/`credentialRef` resolution (names
only), and the hybrid vector-coverage gate. Artifacts under
`<output>/<experiment>/`: immutable `manifest.json`, final `report.json`
(`qualityGate: not_evaluated` — grading is AP-05), per-trial `trial.json`,
`request.json`, adapter `events.ndjson`/`result.json`/`stderr.log` and
`host.log`. Exit codes: 0 completed, 1 operational, 2 usage, 3
preflight/suite, 4 interrupted. A source-checkout build needs no
environment variables; installed npm/native packages do not ship the host
bridge and need `REPOISE_BENCH_HOST_BRIDGE` (pointing at the bridge file in
a source checkout), and `REPOISE_BENCH_RUNNER` optionally overrides the
suite's adapter executable. Node is required only to launch adapter
processes; orchestration is native. Declared limitations: worktrees are
isolation, not security
sandboxes (hidden-test integrity awaits a host-enforced mechanism, AP-05/
AP-06), no grading/metrics/report export (AP-05), and no `--events` stream
for `bench` yet. Post-review hardening: task/condition ids are restricted
to `[A-Za-z0-9_-]` (trial ids and artifact directories derive from them,
so other characters could make sanitized ids collide); a failed patch
snapshot removes the partially created worktree before surfacing the
error; failures after workspace creation (request write, host bridge,
trial-record write) record an `infra_error` trial with the workspace
cleaned up so the report is always published; and `report.json`
`startedAtUtc` is captured at experiment start. A second review round
fixed three more correctness issues: searches that omit `mode` on a
hybrid/vectors-only pinned server now use the pinned mode instead of
being refused (only explicitly different modes are rejected); the seeded
condition-order shuffle now advances its splitmix64 stream (one draw per
swap, so orders are no longer correlated); and the experiment `id` is
restricted to `[A-Za-z0-9_-]` so it cannot escape the artifact output
root (`.`, `..`). A third review round fixed three more gaps: a
hybrid/vectors-only trial whose trial index has no published vectors now
fails setup as `infra_error` (the pinned MCP server would otherwise
silently degrade to lexical, since `repoise index` exits 0 even when
embeddings fail or stay pending); the host bridge now scans the remainder
of the first stdin chunk for `cancel`, so a request and a cancel line
arriving together (SIGINT during trial setup) no longer lose the
cancellation; and the coordinator now applies a 15-minute default
(900000 ms) when the suite omits `limits.wallClockMs` — the per-trial
wall bound is always in effect (forwarded to the adapter and enforced by
the coordinator's outer watchdog), and `experimentWallClockMs` is
documented as checked between trials rather than a hard cap. A fourth
review round closed two credential-forwarding gaps and a CI failure: the
trial's MCP server env now carries the embedding endpoint/API-key
variables declared in the root config (resolved from the coordinator's
process environment; without them a hybrid query on the pinned server
would silently degrade to lexical), and the provider's `credentialRef`
variable is now forwarded to the adapter's process environment (adapters
read `process.env[<name>]`, so a suite with `credentialRef: "env:FOO"`
and `adapter.env: {}` no longer passes preflight and then runs without
the credential); both are covered by unit tests and documented in
`docs/bench.md`. The host-bridge test file is now prettier-formatted
(CI's prettier check), and `docs/automation.md` now reports
`benchmark: supported: true`, the bench exit-code rows and the bench
signal behavior. A fifth review round fixed a plaintext-secret leak in
`request.json` (the `mcp.env` values are now redacted in the artifact — names
kept, values replaced with a placeholder — and the unredacted request goes
only to the host bridge over stdin; the recorded `requestHash` hashes the
redacted form), the snapshot dirtiness check now also excludes the output
root (running `repoise bench` from the repository root with the default
in-repo `output.dir` no longer fails the second invocation with
`working tree is dirty`; an output directory equal to the repository root is
rejected), the hybrid vector gate now also fails on `embedding.failed > 0`
(a partly failed embedding run is never recorded as hybrid), and generated
trial-id collisions across the task/condition boundary (e.g. task `a` +
condition `r0-x` versus task `a-r0` + condition `x`) are rejected in suite
validation. `README.md` and `docs/architecture.md` now document `repoise
bench` and the `repoise mcp --search-mode` pin. A sixth review round closed
an untrusted-content execution gap: the host bridge is now resolved from the
source checkout the running executable was built from (walked up from
`current_exe()`) instead of the user's current directory, so a benchmarked
repository that commits its own
`packages/agent-adapter-protocol/bin/adapter-host-run.mjs` is never executed
automatically with adapter credentials; a resolved bridge inside the
benchmarked repository root is rejected (the explicit
`REPOISE_BENCH_HOST_BRIDGE` override remains an operator choice). A seventh
review round fixed a dirtiness-check bug: the snapshot check now runs `git
status --porcelain --untracked-files=all`, so each untracked file is listed
individually — under the default `normal` mode git collapses an untracked
parent directory into one ancestor entry (e.g. `?? .bench/`), which is not a
descendant of a nested in-repo output root (or cache root), so every run
after the first (including `--dry-run`) failed with `working tree is
dirty`; sibling untracked files beside the output/cache root still count as
dirt. It also corrected the environment wording (installed npm/native
packages do not ship the host bridge and need `REPOISE_BENCH_HOST_BRIDGE`)
and a stale `request.json` comment in the artifact layout. An eighth
review round fixed two coordinator correctness gaps: `timings.agentMs`
now actually uses the host-measured `elapsedMs` (the bridge reports it as a
fractional millisecond value, which `as_u64` silently rejected, forcing
the coordinator's own span — including Node/bridge startup — to be
recorded instead), and the pre-trial drift check now re-hashes the
recorded `patchFile` (a mid-experiment patch edit would otherwise
silently change the snapshot for later trials), and preflight now also
enforces that `patchFile` lives outside the benchmarked repository. A
ninth review round fixed two more encoding/parsing bugs: the host
bridge now puts its stdin in stream-level UTF-8 mode, so a multi-byte
character straddling a pipe read boundary in a large non-ASCII request no
longer decodes to U+FFFD (before, the adapter could silently receive a
prompt that differed from the recorded `requestHash`), and the snapshot
dirtiness check now runs `git status --porcelain -z`, so paths with spaces,
tabs, quotes, backslashes or (default `core.quotePath`) non-ASCII bytes are
no longer C-quoted into unrecognizable entries that made the runner's own
artifacts count as dirt on every run after the first (renames are parsed
from the NUL-separated origin field). See
[docs/bench.md](docs/bench.md) and
`docs/plans/complete/agent-platform/04-benchmark-runner.md` for the implementation
record.

### Benchmark grading and reports (AP-05)

`repoise bench` now grades completed trials and publishes a report
(AP-05; the runner, preflight and artifact layout from AP-04 are unchanged).

- Suite schema v1 gains optional fields: `tasks[].evaluator`
  (`deterministic` checks over the recorded answer and declared artifact
  files; `humanRubric`/`modelJudge` declared-only), `tasks[].category`/
  `heldOut`/`allowedPaths`/`expectedArtifacts`, top-level `gates`
  (baseline, `minCorrectness`, `correctnessMargin`, declared `improvements`)
  and `report` (task-cluster bootstrap iterations/seed). The checked-in
  suite schema documents them; the new grading record schema is checked in at
  `schemas/bench-grading.v1.schema.json`.
- Post-trial grading writes `trials/<id>/grading.json` (data-only; nothing
  read from transcripts or the repository is ever executed; a trial's own
  assertions and exit code are never treated as correctness). Deterministic
  checks are literal-substring assertions (`containsAny`/`containsAll`/
  `notContains`, case-sensitive; alternative correct evidence accepted, exact
  equality deliberately not the default). Censored trials (timeout, adapter
  failure, cancellation) are `not_graded`, never graded as wrong answers.
  Operator-written manual grading records are kept verbatim (never relabeled
  as engine records, so repeated rebuilds never lose them); engine records
  carry `source: "engine"`; an invalid or unparseable manual record is
  `evaluator_error` and the engine never clobbers the operator's bytes in
  place - the operator's file is preserved at
  `trials/<id>/grading.json.rejected` and the recorded error is re-emitted
  verbatim across rebuilds until a valid manual record replaces it; the
  same preservation applies when the engine replaces an operator record
  left on a censored trial (preserved, not honored - a graded result
  cannot stand behind a censored trial). An
  absent answer or artifact fails every check (including `notContains`), so
  a trial that produced no answer is `fail`. Declared artifact files and the
  adapter event log are read only when they are regular files (symlinks and
  devices are treated as not produced) and with a size cap.
- The published report (`report.json`, schema v1) is rendered to `report.md`
  from the same object: status/grading counts (every planned slot lands in
  exactly one bucket), overall/per-condition/per-task grading summaries with
  the held-out split, and - only when `gates` is predeclared - paired
  per-task analysis (median paired difference/reduction over tasks with
  metric-eligible data on both sides - only completed and timed-out trials
  contribute values; other trial statuses are excluded and counted in the
  per-task exclusion reasons - zero-baseline undefined-reduction note,
  task-cluster
  bootstrap percentile interval with recorded seed/iterations, labelled
  success-only medians, timeout rate) plus the predeclared quality gate
  (`pass`/`fail`/`insufficient_data`/`not_configured`): the overall status
  is `pass` only when every declared check was evaluated and passed; a
  speed gain with insufficient correctness data is `insufficient_data`,
  never `pass`. The gate is descriptive evidence against predeclared
  thresholds, not a product gate.
- `repoise bench report --experiment <ID> [--output <DIR>] [--json]` re-grades
  recorded trials and rebuilds the report offline from the frozen manifest
  and trial artifacts (no worktrees, no agent processes, no network);
  `not_started` slots are persisted as `trial.json` so rebuilds see the same
  counts (not-started slots are never re-graded, matching the run), and the
  run's status and `failureReason` carry over from the previous report.
  Exit `0` on success, `1` when the experiment is missing or incomplete.
- Fixes from the PR review round: a `humanRubric` evaluator without a rubric
  (or with a blank one) now fails suite validation, matching the schema,
  docs and `Evaluator` doc comment (the checked-in suite schema enforces it
  with an `if`/`then` rule); the `bench report` rebuild path re-validates
  the frozen manifest's task, condition and gate declarations - the
  manifest is untrusted input, so a copied or hand-edited manifest fails
  with a configuration error instead of panicking a grader or reading
  outside the trial directory - and structural check-declaration defects
  (unknown scope/type, artifact scope without a name, non-plain artifact
  names) degrade to an `evaluator_error` instead of panicking; trials
  killed by the coordinator's outer watchdog (`infra_error` or `cancelled`
  with reason `host_watchdog`) are counted as censored timeouts that keep
  their recorded capped agent time, not as infra failures.
- Further review fixes: the `bench report` rebuild path now also re-validates
  the manifest's `report` spec, so a `bootstrapIterations: 0` spec fails
  with a configuration error instead of reaching a percentile of an empty
  bootstrap sample (the percentile helper now returns NaN - JSON `null` - on
  an empty draw as defense in depth); a broken relative link in
  `scripts/eval/RUNBOOK.md` and the stale "AP-05 not implemented" boundary
  line in `docs/agent-adapters.md` are fixed.
- Further review fixes: the Cline adapter now records the agent's final
  message text on the `final` event's `data.answer` (null when no final
  text was produced) - the convention is described in the adapter-events
  schema and the agent adapter guide - so deterministic answer grading
  works end to end with the shipped adapter; `repoise bench` now takes the
  report's task provenance from the frozen manifest instead of re-reading
  prompt files after the trial loop, so a prompt file deleted mid-run
  cannot prevent the report from being published; the `docs/bench.md`
  check description names the required `id` and optional `note` fields and
  shows an example check.

Declared limitations: `modelJudge` has no model-judge runtime (the tested
model never grades itself); worktree isolation is not a security sandbox for
hidden tests; no v1 replay-evidence export is offered (the existing import
validation is unchanged). See [docs/bench.md](docs/bench.md) and
`docs/plans/complete/agent-platform/05-suites-metrics-reports.md` for the
implementation record.

### Cross-worktree cache reuse

Linked worktrees of one local Git repository now share reusable parse
artifacts and embedding vectors through a versioned family store under the
canonical Git common directory (or under `families/<familyId>/` in an
cache root explicitly configured via `cacheDir`/`REPOISE_CACHE_DIR`). Each
worktree still publishes its own scope-bound index; search, exact reads,
and freshness always reflect the destination checkout. `repoise index` in a
new worktree at the same commit skips parsing for unchanged compatible files
and, for a completed compatible embedding profile, makes no
document-embedding provider calls; small deltas reparse only affected
files. New configuration option: `sharedCache` (default `true`) opts a
worktree out of sharing. `doctor` and `status` report the sharing state and
store location; `index` reports `files_family_reused`/`chunks_family_reused`
(JSON and human output). No new environment variables, secrets, or migration
steps are required; existing caches keep working (legacy checkout-local
embedding caches are re-embedded on the next embedding build, not migrated).
Worktree-scoped `purge` removes only that scope's member registration under the
family store's `members/` directory (sibling shared data untouched); `purge --all`
removes `families/` only when the family store lives under the explicitly
selected cache root. Independent clones, plain directories, and opted-out worktrees
remain isolated. See [storage.md](docs/storage.md#cross-worktree-reuse-family-store)
and [agents.md](docs/agents.md#linked-worktrees).

## 0.1.0-alpha.1 — 2026-10-15 (functional preview)

First functional preview of the offline repository knowledge engine. Includes
incremental docs/code indexing, lexical search, hash-validated exact reads,
related links, freshness checks, watch, managed agent guidance, stdio MCP,
and opt-in embeddings/history. See [preview readiness](docs/publish/alpha-1-readiness.md).

Published under npm `next` with native prerelease archives; the `latest`
dist-tag was promoted to this version on October 19, 2026. See the
[publication record](docs/publish/alpha-1-status.md). This is not a stable v1 release. Pilot runs verify basic operation but do not
establish consistent agent speed or token savings. Exact-identifier ranking
and agent retrieval strategy remain follow-up work. Fresh registry installation and CLI/MCP checks passed on macOS ARM64;
Linux/Windows public-install checks remain unmeasured. Cargo registry
publication stays disabled.

### Pre-v1 reliability — pilot follow-up

- `doctor` now reports the invocation it actually used: the selected snapshot
  mode (`--committed` for Git roots is honored) and the configuration paths
  actually selected (repository defaults or external `--config` /
  `--local-config` overrides), each with an existence status. Before, `doctor`
  always reported the working-tree mode and only the in-repository config
  paths, regardless of the flags used with the other commands.
- Lexical search now enforces its documented match semantics: the FTS5
  expression joined per-term field groups with top-level `AND` but left the
  per-term `OR` groups unparenthesized, so FTS5 precedence (AND binds tighter
  than OR) silently relaxed multi-term queries to a mostly-OR match. Each
  per-term group is now parenthesized (the history lane already carried this
  fix), so every whitespace query term must co-occur in one chunk again.
  Query-time only: no reindex required, but broad queries may return fewer,
  more precise results than before.
- The in-tuning eval query Q-ACME-TEST-01 was relabeled
  ("computeBatchId stable first second" → "computeBatchId first second"):
  its "stable" term occurs only inside the camelCase identifier
  `testBatchIdIsStable`, which the unicode61 tokenizer keeps as one token,
  so under the strict all-terms semantics the old query could match no
  chunk. Gold evidence and held-out set are unchanged. G3 recall uses held-out
  conceptual queries; other gates use exact-lookup queries, policy/isolation
  evidence, timings, or separately supplied agent/hybrid measurements. (Follow-up candidate:
  query-side camelCase term splitting for identifier substrings.)
- Copied fixture/test-data content (paths under `fixture(s)`, `testdata`,
  `test-data`, `test_data`, `test_fixtures`, `__fixtures__`, `__snapshots__`)
  stays fully searchable but no longer gains the current-document ranking
  boost, so copied test input can no longer claim current-document authority
  over real sources for ordinary queries. Classification is unchanged, and
  explicit path filters targeting fixtures still work. Query-time only: no
  reindex required.
- Agent retrieval guidance is now shipped in the MCP tool descriptions, the
  CLI help text and the managed `AGENTS.md` snippet (template version bumped
  to 1.2.0): strict lexical matching (every whitespace query term must
  match the same chunk's searchable fields, so broad natural-language
  queries may return zero results even when relevant evidence is indexed —
  retry with shorter identifier/symbol queries and path/role filters,
  bounded), excerpts locate evidence and are not the source (use exact
  reads of source ids); short excerpts do not mean the source is
  unavailable; paginate by passing the response's `next_cursor` value as
  the next request's cursor; empty `related` results do not establish
  missing evidence; fall back to targeted filesystem reads only when
  indexed evidence is insufficient and explain the limitation; keep
  retries bounded. Existing overlays pick up the new snippet with
  `repoise overlay update` (unchanged blocks migrate; user-modified blocks
  report conflicts, user content is preserved).

### Card K6 — package release validation

- The packed-consumer smoke now runs the same offline docs+code fixture (one Markdown doc
  plus TypeScript and JavaScript sources) against both the installed npm launcher and the
  extracted native archive, from outside the source checkout: `init --preset
  docs-code-lexical`, `index`, lexical `search` (doc and code hits), exact `read` back of
  the expected text, `status` and `check --fresh` reporting a fresh index, and a stdio MCP
  session (initialization, initialized notification, tool discovery, search/read/status
  with CLI-identical results, clean shutdown on stdin EOF) that verifies the default server
  hides and refuses `refresh_project_knowledge`.
- Native functional runs use no Node/npm on PATH; the generated cache is isolated per
  install; every subprocess is bounded by timeouts and the temporary tree is removed on
  exit, so broken MCP behavior cannot hang CI.
- The versioned config schema (`schemas/repoise.config.v1.schema.json`) ships in both
  installed forms and is verified against the committed source; tree-sitter parser grammars
  remain compiled into the native binary.
- The smoke runs in the Linux/macOS/Windows CI and release matrices. Fresh
  public-registry install checks passed on macOS ARM64; Linux/Windows
  registry installs remain unmeasured.

### Card K6 — offline lexical evaluation harness

- New reproducible offline evaluation path under `scripts/eval/`: frozen synthetic
  public-safe fixtures (`acme` docs+TypeScript/JavaScript, `docs-site` docs-only,
  `pyrepo` Python fallback) with a SHA-256 file freeze and deterministic replayed git
  history, plus a dirty-worktree phase.
- 60 labeled queries (48 in-tuning, 12 held-out) across exact identifiers, unknown
  concepts, current-vs-plan conflicts, history, code behavior, tests,
  unsupported-language fallback, docs-only, dirty/stale, secret-policy and
  cross-scope categories; gold evidence is path/range/revision.
- `node scripts/eval/harness.mjs` runs the built native CLI per corpus/preset,
  measures cold build, corpus/index sizes and warm search p50/p95 (per CLI
  invocation), validates provenance for every returned hit (path presence and
  SHA-256 content hash at the snapshot revision, line-range bounds, history
  revision/affected-path checks), compares the `docs-only` and `docs-code-lexical`
  presets on the acme corpus, and writes machine-readable `results.json` plus
  `report.md` (in git-ignored `eval-results/`) with hardware, versions and declared
  settings.
- Master-plan section 11 gates are reported as measured-passed, measured-failed or
  unmeasured: exact lookup, policy, held-out recall@5 and warm latency are measured;
  token reduction, code vectors and hybrid latency stay unmeasured — the harness
  never infers token savings from retrieval size and never runs hybrid without an
  operator-configured embedding provider.
- Validated import format for real agent replay evidence
  (`scripts/eval/replay-evidence.schema.json`, imported via
  `--replay-evidence`): fixed model/tasks/revision/budgets and per-run measured model
  input/output tokens, retrieval output tokens, tool count, elapsed time, task
  correctness; derived values and missing task x condition cells are rejected.
- Shared-cache isolation phase: the harness indexes `acme` and `docs-site` into one
  shared cache root, then verifies that foreign source ids are not readable from the
  other scope (both directions, with content inspection), that an in-scope exact read
  returns the exact snapshot text, and that an earlier source id is rejected as stale
  after the indexed file changes; uncovered cases are reported as unmeasured.
- Replay evidence is fail-closed: an invalid or malformed `--replay-evidence` file
  stops the harness with clear diagnostics and a nonzero exit before any indexing work.
  Aggregated token means divide by the summed `sampleSize` (per-sample means); the
  correct rate is labeled task-level; the mean-based reduction is labeled descriptive
  and does not establish the median-reduction gate.
- Harness tests run via `npm test` (`node --test scripts/release/*.test.mjs
  scripts/eval/*.test.mjs`), including regression tests for unequal sample sizes,
  zero-token baselines, task-level correctness, null/duplicate task entries,
  injected isolation leaks and the nonzero-exit import path; the eval JavaScript and
  JSON files are covered by `format`/`format:check` without touching the frozen
  fixture bytes. Runbook at `scripts/eval/RUNBOOK.md` (commands, held-out separation,
  pilot evidence workflow, acceptance targets, rollback).

### Card K6 — build/Actions integration examples

- `docs/build-ci.md` consumer guide for the offline `docs-code-lexical` baseline: pinned
  registry install using the explicitly labeled `REPLACE_WITH_VALIDATED_RELEASE_VERSION`
  placeholder (at that PR the functional release was not yet published; the
  build/CI guide documents the current pin), reviewed schema-version-1 config,
  a generated-cache ignore policy
  (`.repoise/` and `repoise.local.json` stay out of version control), opt-in
  owner-invoked package scripts, and the `index`-then-`check --fresh` sequence with stable
  failure-stop exit codes. It separates the agent's startup refresh (one explicit
  `repoise index` on the real checkout, then optional `watch`) from a separately started
  `watch` loop.
- `examples/ci/repoise-ci.example.yml`: a minimal GitHub Actions `push`/`pull_request`
  example — pinned install, exact checked-out SHA (`${GITHUB_SHA}`),
  `persist-credentials: false`, read-only `contents: read`, an isolated per-run cache
  (`REPOISE_CACHE_DIR`), rebuild-from-scratch with no cache restore or artifact upload,
  `index` then `check --fresh`, no `pull_request_target` and no consumer package scripts.
  It reports the revision and the shallow-history limitation and is an example, not an
  active workflow (it does not live in `.github/workflows/`). `examples/consumer/package.json`
  shows the opt-in scripts; `examples/README.md` indexes both.
- The guide documents that a CI index does not refresh an agent's local checkout and states
  the future cache-compatibility and private-corpus isolation requirements without
  implementing cache/artifact machinery.
- Validated locally (not in GitHub): the documented command sequence was run in a
  disposable synthetic consumer against the current packed package (installed npm launcher
  and the native binary packed into the native archive, no Node on PATH), offline, with an
  isolated cache; `check --fresh` exited `3` before indexing and `0` after. The GitHub
  workflow was not executed and no workflow was activated. This is documentation/examples
  only; it does not mean the live pilot or the master-plan section 11 adoption gates passed.

### Card K6 — stdio MCP server

- `repoise mcp`: stdio MCP server (JSON-RPC 2.0 on stdin/stdout) exposing
  `search_project_knowledge`, `read_project_knowledge`, `related_project_knowledge`
  and `project_knowledge_status` over the same shared services as the CLI, with
  identical versioned response schemas. Read-only by default; stdout carries protocol
  traffic only (diagnostics go to stderr); clean shutdown on stdin EOF.
- Explicitly opt-in `refresh_project_knowledge` tool: enabled by the new
  `mcp.refresh` configuration key (schema v1) or `--allow-refresh`; it runs the same
  bounded, transactional index operation as `repoise index` for the configured scope.
- `read` gained an optional output-token budget with line-boundary truncation and a
  `truncated`/`output_tokens` report (CLI `--max-output-tokens`, default 2500 for
  exact reads).
- Scope-bound server: optional `repoId`/`worktreeId`/`snapshotMode` arguments must
  match the server-configured scope (refresh included); opaque source ids remain
  references, not authorization. Tool arguments are validated against the advertised
  input schemas (required fields, types, enums, numeric bounds,
  `additionalProperties`) before any service call; violations are JSON-RPC
  `-32602` invalid-params errors.
- Client registration guide at `docs/mcp.md` (generic stdio server shape;
  client-specific examples remain unverified).

### Card K4 — offline code indexing

- Offline code indexing: pinned tree-sitter TypeScript/TSX and JavaScript/JSX structural
  chunks with exact source line ranges, enclosing scope/symbol context and labeled
  oversized splits; a deterministic line-window fallback chunker for other code languages.
- Per-generation code symbol records (function/method/class/interface/enum/type-alias
  and more, with parent symbols and export flags) and syntactic reference edges
  (imports resolved against the generation's file set; calls/references as bounded,
  honest-confidence hints).
- FTS lexical search over code chunks with new symbol/context columns; embedding scope
  (`docs` or `docs+code`) selects which corpora receive vectors.
- Index schema v3 with an additive, idempotent migration (column adds guarded, FTS table
  rebuilt only when predating the `context` column); `repoise index` reports symbol and
  reference-edge totals.

## 0.1.0-alpha.0 — 2026-10-04 (native release; npm rollout partial)

- Native Rust CLI scaffold: greeting, help, version and argument errors.
- Thin npm launcher and Linux x64 GNU, macOS ARM64, Windows x64 MSVC binary packaging.
- Manual release preparation, staged npm publishing, native release drafts and SHA-256 checksums.
- MIT license. Indexing, persistence and MCP remain unimplemented.

Native archives and Linux/macOS npm binary packages are public. Windows npm name review and
main npm launcher publication remain pending; see [publishing status](docs/publish/alpha-0-status.md).
