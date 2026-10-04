# Repoise v1 — master implementation plan

Status: proposed; planning only, no runtime implementation.
Project: [billy-the-ape/repoise](https://github.com/billy-the-ape/repoise).
Target: a standalone Rust engine/native CLI, initially distributed through npm.
Canonical planning location: `docs/plans/v1_master_plan.md` in this repository.
Origin: [ai-gateway planning PR #221](https://github.com/billy-the-ape/ai-gateway/pull/221),
migrated from commit `d235d9da50087217d94a2fe8188195e8a87e1779`.
Pilot baseline inspected: ai-gateway main `4be9f83dba57f79f6c0f6086cefef90c5eca71e1`.
The separate [documentation PR #220](https://github.com/billy-the-ape/ai-gateway/pull/220)
is pilot context, not a dependency of Repoise. The repository currently contains documentation
and a Rust .gitignore; no CLI, Cargo workspace, indexes, package releases or benchmarks exist.
Read [AGENTS.md](../../AGENTS.md) for repository working rules.

## 1. Outcome and boundaries

Give an AI coding agent a cheap, reliable way to discover where repository knowledge lives:
current docs, plans, execution records, ADRs, readable configuration, optionally code and Git
history. Optimize successful task completion per context token, not the quantity of indexed text.
Keep filesystem search and exact source reads available at every stage.

The deliverable is a generic repository knowledge tool, not a gateway-specific chat feature.
It must work on a documentation-only repo, an arbitrary language repo, and a monorepo without
requiring ai-gateway, GitHub, an LLM chat provider, or a vector database service.

Implement Repoise in this repository as a Rust engine and native CLI, designed for
language-, framework-, source-control-, hosting-, and package-manager-independent operation.
npm is the first distribution target, not the engine's runtime or API boundary. ai-gateway
consumes a pinned release; no indexer implementation belongs in ai-gateway.
Expose a Rust library, versioned CLI JSON contracts, and a stdio MCP entry point. An optional versioned
overlay contains config, agent wiring examples, and concise documentation hooks; it points at
the installed package rather than copying implementation into consuming repositories.
Product, CLI and repository name: Repoise (`repoise`); owner: `billy-the-ape`.
Use `repoise.config.json` and a `repoise` cache namespace in proposed contracts.
Registry package/crate names or scopes, license, and public release timing remain to be finalized.
The naming check found no exact package listings, but reserves nothing. Repowise is a similarly
named existing developer tool; retain clear product descriptions and perform release naming checks.
This document is the canonical master plan; ai-gateway retains a migration pointer.
Do not infer authorization to publish a release from this planning change.

Automatic maintenance means refreshing searchable code/document evidence and reporting freshness.
It does not mean rewriting source, manufacturing authoritative docs, or proving existing prose
matches runtime behavior. Curated documentation maintenance remains part of each change's
definition of done; optional drift diagnostics report evidence for review.

### Success conditions

- An agent can find an unknown concept, inspect an exact source range, and verify its revision.
- Exact identifiers/paths work without embeddings; concepts benefit from semantic search.
- Current implementation and historical proposals remain distinguishable.
- Branch/worktree data cannot leak into another branch's answer unnoticed.
- Unchanged text does not need re-embedding on every commit.
- Installation and uninstall do not overwrite existing AGENTS.md or project tooling.
- Missing embeddings/index/network degrade to lexical/local discovery with explicit status.
- Optional code indexing earns its complexity through measured retrieval/task improvement.

### Non-goals for the initial release

No autonomous code editing, documentation generation, PR posting, hidden prompt injection,
global cross-user memory, full compiler-grade call graph, binary/OCR ingestion, or automatic
indexing of every historical file revision. No alteration of the Home Lab RAG contract.
No background daemon or scheduled job installed without an explicit operator action.

## 2. Existing ai-gateway context and reuse assessment

| Existing module | What exists | Decision |
| --- | --- | --- |
| [knowledgeFiles.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/rag/knowledgeFiles.ts) | Recursive regular .md discovery | Do not reuse as generic Git-aware intake/security policy |
| [chunk.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/rag/chunk.ts) | Heading/character splits | Useful fixture baseline; use structural parsers for new tool |
| [indexStore.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/rag/indexStore.ts) | Version-1 JSON, full rebuild, partial embeddings | Preserve; new index has separate storage/schema/lifecycle |
| [retrieval.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/rag/retrieval.ts) | Cosine search | Mathematical baseline, not the full retrieval architecture |
| [embeddingInput.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/rag/embeddingInput.ts) | Model input profiles | Adapt the pattern; fingerprint model/profile/tokenizer separately |
| [rag-index.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/scripts/rag-index.ts) | Home Lab CLI/config wiring | Keep pnpm rag:index unchanged |
| [profiles.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/workflows/profiles.ts) | Route-attached context/tool grants | Future integration point, not part of standalone core |
| [capabilities.ts](https://github.com/billy-the-ape/ai-gateway/blob/4be9f83dba57f79f6c0f6086cefef90c5eca71e1/src/orchestrator/capabilities.ts) | Validated capability operations | Optional gateway adapter only after core contracts settle |

Do not import gateway configuration, database stores, chat orchestration, or Home Lab paths
into the standalone core. A pilot may use the same OpenAI-compatible embedding endpoint via
an independent provider adapter and separate configuration.

## 3. Architecture and interfaces

Suggested standalone layout (names are proposed contracts):

| Component | Responsibility |
| --- | --- |
| core/discovery | Git/non-Git inventory, include/exclude policy, scope checks |
| core/documents | Text decoding, metadata, Markdown/text/config parsing |
| core/code | Optional grammar registry, symbols, structural code chunks |
| core/history | Optional bounded local commit and remote PR enrichment |
| core/index | Schema, generations, manifest, incremental publication |
| core/search | Exact/lexical/vector fusion, filtering, deduplication, budgets |
| core/read | Revision-pinned reads and explicit working-tree reads |
| adapters/embeddings | Optional local/OpenAI-compatible embedding transport |
| cli | init, doctor, index, watch, check, search, read, status, purge |
| mcp | Read-only search/read/status/related over stdio |
| templates | Overlay manifest, sample config, agent integration snippets |
| eval | Labeled queries, ablations, task replay and token/cost reports |

Choose Rust for the engine, CLI, and stdio MCP. Organize a Cargo workspace with core, adapter,
CLI/MCP and evaluation crates; wrappers contain packaging/client glue only. Pin an MSRV/toolchain,
dependency versions and supported target matrix. Use cargo fmt, clippy and relevant cargo tests
before commits to this repository; wrapper files follow their own checked-in formatting config.
Native CLI operation must not require Node, npm, Python, ai-gateway or an embedding server.
CLI and MCP call the same Rust service methods and return the same versioned schemas.
Rust consumers may use a public crate; other languages use subprocess JSON or MCP initially.
Do not promise an in-process TypeScript library or stable Rust binary ABI. A future thin SDK
or FFI binding requires a separate compatibility design, not a duplicate indexing engine.

### Portability contracts and staged adapter support

Separate source-control systems (Git, Mercurial, SVN, etc.) from hosts (GitHub, GitLab,
Bitbucket, Azure DevOps, self-hosted services). A Git repository on another host needs no
GitHub adapter to index locally. An ordinary directory requires neither VCS nor remote host.

| Boundary | Contract | Initial support |
| --- | --- | --- |
| Source inventory/snapshots | Enumerate/read exact source, snapshot identity, containment | Filesystem + Git |
| Source control | Opaque revisions, optional history/change detection/workspaces | Git; other VCS deferred |
| Ignore policy | Explainable rule sources and adapter-owned native rules | Package globs + .gitignore/Git excludes |
| Language/parser | Grammar, symbols, chunks, versions, explicit fallback coverage | TS/JS first; Rust self-hosting fixtures; generic text fallback |
| Hosting/breadcrumbs | Validated source URLs and optional change-request associations | GitHub optional; other hosts deferred |
| Embeddings | Fingerprinted local/runtime or endpoint provider | Offline lexical baseline; optional adapters |
| Distribution | Install a versioned native executable and required assets | npm wrapper first; independent native build/run |
| Agent/CI integration | Versioned CLI JSON and stdio MCP | Generic contracts; client/platform examples optional |

Define capability flags rather than require every adapter to provide Git-like branches, atomic
snapshots, history, diffs or immutable reads. Unsupported operations return explicit status.
Model provenance as sourceKind + revisionId + content manifest/hash; Git adapter validates SHA
semantics, filesystem uses manifest hashes, future VCS adapters own their revision semantics.
Never encode assumptions that every revision is a 40-character hash or every change request a PR.
Store hosting fields in optional namespaced metadata, not mandatory GitHub database columns.

Ignore inheritance is adapter-selected: Git uses its native rules; filesystem may honor
.gitignore as a familiar convention; future VCS may contribute their own rules. Config remains
the portable corpus policy. URL builders/credential access/history mapping belong in host
adapters, not ranking/chunking. Git-specific sections below describe the first adapter only.

Parser metadata must not require package.json, tsconfig, src/, or a particular framework.
Language detection/configurable roots work on mixed-language repos and docs-only folders.
Syntax plugin contracts are versioned Rust interfaces initially, not an unsafe arbitrary shared
library ABI or a promise of third-party hot loading. Pin grammar/query versions and provide
honest fallback behavior. Optional semantic enrichments cannot make generic indexing unavailable.

Contract tests include non-Git directories, non-GitHub Git remotes, docs-only and mixed-language
roots, repos without Node manifests, and a synthetic non-Git revision adapter. This fake adapter
tests neutral contracts without promising production support for additional VCS at launch.
Do not build every adapter now: implement these seams and prove them with fixtures.

Local-first storage proposal: SQLite metadata and FTS5, plus embeddings associated with chunks.
Start with bounded exact cosine search for small indexes, behind a vector-store interface.
Add an ANN/store adapter only when measured latency/memory justify it; do not require a service
for a few thousand documents. Inspect SQLite FTS5 support in doctor; fail clearly if absent.
Separate vector collections by model/profile/dimension; never compare incompatible spaces.

## 4. Inventory and safe intake

Local agent commands default to working-tree scope; CI uses an explicitly resolved committed
snapshot. Committed mode reads blobs from one resolved Git commit, not mutable files while hashing.
For a non-Git folder, build a filesystem snapshot manifest and validate hashes before publication.
Report a changed-during-scan snapshot instead of claiming commit consistency.
Use Git argument arrays and NUL-delimited paths; never construct shell strings from repo paths.
Handle spaces, Unicode, case collisions, renames, deleted files, and detached HEAD explicitly.

### Include policy

- Inventory all eligible readable text, not only Markdown, but classify its role before indexing.
- Initial parsers: Markdown/MDX as text, .txt/.rst/.adoc, and relevant JSON/YAML/TOML/INI config.
- Extensionless README/LICENSE/instruction files can use text detection and policy rules.
- Code files are an optional separately classified corpus; grammar support is explicit.
- Unknown valid UTF-8 text can use a generic paragraph parser with a fallback marker.
- A proposed 1 MiB default file limit bounds intake; override deliberately. Skip diagnostics
  identify relative path and reason, never content. Binary/unsupported encoding is not silent success.

Exclude dependencies, vendored/generated outputs, minified bundles, maps, lockfiles, caches,
build artifacts, runtime data, index output, .git internals, logs, secret/private-key files,
and known environment credential files by default. These may be readable text but mostly add
noise or confidential material. Explicit config selects safe template config, e.g. .env.example.
Use configurable path globs plus content secret detection before sending anything to an
embedding endpoint; scanning is defense in depth, not a guarantee of complete secret detection.
Secret deny rules cannot be bypassed by a broad include glob; doctor explains effective policy.

Do not follow symlinks/submodules/LFS pointers by default. Opt-in submodules are separate repo
scopes. Canonical-path containment applies to filesystem reads and cache directories.
Ignore rules apply to all discovery modes; never infer safety from Git tracking.
Honor root and nested .gitignore rules by default, including Git-style negation and directory
semantics, evaluated from the selected commit in committed mode. Also honor applicable local
Git excludes in working-tree mode; record local policy differences in the config fingerprint.
Non-Git folders support the same .gitignore syntax when those files exist.
Package defaults exclude noisy/generated/secret paths even without a .gitignore.
An explicit setting can disable Git ignore inheritance or narrowly re-include ordinary ignored
content; secret deny rules and containment checks remain enforced. A broad include never
implicitly overrides ignores. Doctor/explain must show the winning rule and its source.
Changes to ignore/config rules invalidate inventory and remove newly excluded served records
on the next successful publication, even when source files themselves did not change.

### Authority and lifecycle classification

Use configured path rules and optional front matter to assign:
`instruction | current-doc | decision | plan | execution-record | historical | code | config`.
Separate role from lifecycle: `proposed | accepted | implemented | superseded | unknown`.
Record whether metadata was explicit or inferred. Priority is task-dependent: a requested plan
should not rank below unrelated current docs merely because it is a plan.
Never claim a plan is implemented from a checkmark, merged PR, or directory name alone.
Maintain existing docs unchanged; metadata can live entirely in index config.

## 5. Chunking documents and code

### Prose

Parse Markdown structurally so headings inside fenced code are not headings. Preserve heading
ancestry, links, adjacent explanatory paragraphs, lists, tables, and fenced blocks.
For other formats, use the appropriate structural/paragraph splitter with source offsets.
Aim initially at 300–700 tokens, hard ceiling 1,000 or the provider input limit if smaller.
Tune through evaluation, not universal claims. Large tables/code blocks require labeled splits
with repeated header/context, exact ranges, and parent references.
Keep metadata context (path/title/heading) in the embedding input, not duplicated in read output.
Model-specific tokenizer/profile selection governs limits; character count is only an estimate.

### Code: useful but optional

Exact symbol and path retrieval comes first. Semantic code embeddings address questions where
the agent knows the behavior but not the identifier. The recommended preset disables code
vectors; the recommended default preset includes docs/config and code symbol/lexical indexing
for supported languages, with labeled fallbacks. A docs-only preset remains available.
Semantic code vectors are a separate opt-in.
For the ai-gateway pilot compare docs-only, docs+code lexical, and docs+code hybrid.

Use Tree-sitter grammar adapters initially for TypeScript/JavaScript; extend via pinned language
plugins to Python and other languages only with fixtures. Generic line-window fallback keeps
unsupported languages usable and labels parsing coverage honestly.

Chunk at function/method/type/class/module boundaries. Include attached doc comments, signature,
enclosing symbol, decorators, and relevant import names in compact embedding context.
Split large functions by statement/block boundaries while retaining parent/signature.
Do not embed an entire dependency closure or repeat whole imports in every read.
Record parser errors; fall back for affected ranges rather than dropping the whole file.

Build declaration/export/import links and local reference hints. Syntax alone does not resolve
dynamic dispatch or prove a call graph; label uncertain edges. Compiler/LSP enrichments are
optional future adapters. Definitions, related tests, imports, and parent scopes are bounded
related results, fetched on demand. Rank implementation and tests distinctly.

Avoid generated LLM summaries in the initial index: they increase build cost and can introduce
unsupported claims. Benchmark a code-capable embedding model against the available text model;
separate profiles/collections when needed. Choose based on recall/cost, not branding.

### Separate document and code pipelines

Docs/config use heading/paragraph/field structure and text-oriented lexical fields. Code uses
language-aware symbols/signatures/blocks and code-oriented lexical fields; keep corpus role,
parser/chunker versions, coverage, and retrieval weights distinct. History is a separate lane.
A shared persistent store and unified search interface can query these logical indexes.
Semantic profiles may use the same model only when benchmarks support it; incompatible models
never share a vector space. Fuse per-corpus/profile ranked lists, not raw similarity values.
Cross-corpus queries return a bounded mix; path/symbol intent and explicit filters control routing.
Do not require two models or vectors for the offline baseline.

## 6. Provenance and storage contract

Every result must trace to exact source, not just an embedding score.

| Record | Required fields |
| --- | --- |
| Repository | repoId, root binding, sanitized remote identity (optional), visibility/scope |
| Snapshot | snapshotId, sourceKind, opaque revisionId when available, content manifest hash, mode, optional branch/workspace hint, dirty overlay digest |
| File | path, blob/content hash, role/lifecycle and classification source, language, parser version |
| Chunk | opaque chunkId, parentId, exact line/byte ranges, heading/symbol ancestry, text hash |
| Embedding | input hash, provider/model revision or operator artifact ID, dimension, profile/tokenizer version |
| Generation | schema/parser/config fingerprints, inventory coverage, build timestamps, publication state |
| History item | adapter-qualified revisionId, optional parents, affected paths, bounded summary; optional verified host/change-request association |

Chunk identity incorporates repo scope, file content and structural location; content-addressed
embedding cache identity uses normalized embedding input plus the full embedding fingerprint.
A rename may reuse vector input only when contextual path input has not changed.
No positional counter is the only identity. Location metadata still updates on line moves.

Return GitHub permalinks only from a validated GitHub remote and exact SHA/path/range.
For other hosts use configured URL adapters; absent one, return repoId/path/revisionId, not a guessed URL.
Strip embedded credentials from remotes before storage/logging.
Line ranges refer to the stated snapshot, never silently to latest main.

### Persistent layout and lifecycle

Store derived indexes on disk; memory only holds disposable query/parser caches.
Default cache root uses the platform user cache convention (XDG cache on Linux, Library/Caches
on macOS, LocalAppData on Windows), resolved through a tested platform adapter.
Illustrative layout under that root:

| Path | Contents |
| --- | --- |
| repoise/repos/<repoId>/worktrees/<worktreeId>/index.sqlite | Source manifests, snapshot/generation metadata, chunks, symbols/edges, FTS, optional vectors |
| repoise/repos/<repoId>/embedding-cache/ | Optional shared content-addressed vectors, isolated within the repository/access scope |
| repoise/repos/<repoId>/worktrees/<worktreeId>/state.json | Versioned integration manifest and refresh diagnostics; database remains canonical |
| repoise/repos/<repoId>/worktrees/<worktreeId>/tmp/ | Disposable build/import/export staging |

Database table boundaries distinguish docs/config/code/history; separate physical databases are
not required. Embedding profiles/collections remain separate when their spaces differ.
Start with vectors in SQLite if practical; external vector files are an implementation choice
only if generation publication/GC stays atomic. Account for SQLite WAL/SHM sidecars.
Use a local disk, not a shared live network database; export via a consistent backup/checkpoint
operation rather than copying an actively written database file alone.

Repo identity binds the actual local root/access scope; identical remote URLs must not merge
independent private clones. Worktree identity separates branch/dirty state. Root moves require
explicit rebind or rebuild. Committed generations may reuse compatible repository-local vectors
without serving another worktree's dirty text.

An explicit ignored project-local cache directory is supported for containers/portable workflows.
Cache paths cannot affect the eligible source corpus. Commit config, integration scripts, and
curated docs; do not commit generated SQLite/vector indexes or refresh them in pre-commit by default.
They are derived data, create binary churn/merge conflicts, may retain private/deleted content,
and cannot establish correctness merely by appearing in the same commit.

Agent runner startup reconciles/indexes the actual checkout before retrieval, then may start
watch; unchanged content reuses persisted work. A pre-commit freshness check is optional, not
the primary freshness mechanism. CI artifacts are optional accelerators, bound to an exact
source manifest/revision and validated schema/config/parser/provider fingerprints. Reject incompatible
artifacts and rebuild locally. Working-tree generations never claim immutable commit identity.
Cache retention/size ceilings and status/purge commands cover source text and vector storage.

## 7. Incremental indexing and branch correctness

1. Resolve scope and snapshot; load last complete generation for that scope.
2. Enumerate eligible files and compare content/config/parser/embedding fingerprints.
3. Reparse changed files; reuse unchanged embeddings from content-addressed cache.
4. Tombstone deleted/moved records in the new generation; preserve old committed snapshots
   only according to bounded retention policy.
5. Embed with bounded batching, timeouts, retry/backoff, cancellation, and request/input budgets.
6. Validate dimensions, finite vectors, ranges, coverage, and manifest consistency.
7. Publish a complete generation transactionally; readers remain on the old one until commit.

Serialize publishers per repo/scope, not all users globally. A failed build must not publish a
truncated replacement as healthy. An explicit partial generation can publish lexical coverage
with missing vectors reported per file; search reports semantic coverage and falls back lexically.
Never silently replace successful vectors with zeros.

Branch names are mutable aliases, commit hashes are snapshot identity. Worktrees and dirty state
have separate scopes. Local working-tree mode layers modified/deleted/untracked eligible files
over a committed snapshot; exact content hashes establish freshness. It must shadow old code hits
for replaced paths, report dirty status, and never fabricate commit permalinks for dirty text.
Search/read default to the agent's declared current scope, not whichever index built most recently.

On read, validate result scope and source hash. A changed working-tree result returns
`stale_result` with refresh instructions, not unrelated new text. Committed reads stay pinned.
Retention/GC must reference-count shared embeddings and preserve active readers/generations.
Purge removes source text/vectors/cache for a scope and verifies logical absence; do not promise
forensic secure erasure of SQLite files or external backups.

### Granular refresh contract

Staleness can follow agent edits, human/editor saves, formatters, merges/rebases/checkouts,
other processes, or ignore/config changes. File watching accelerates detection but is not a
correctness proof. Watches observe saved filesystem contents, not unsaved editor buffers.

Distinguish reading/parsing a file from updating index records and computing embeddings:
- First implementation reads/hashes and structurally reparses only changed eligible files.
  Reconcile chunks against their previous embedding inputs; update only changed lexical records,
  removed chunks, affected relationships, and necessary location/provenance metadata.
- Embeddings are indivisible per chunk input: regenerate only chunks whose complete input hash
  or provider/profile fingerprint changed. Never patch a vector by embedding just edited lines.
- A one-line edit commonly affects a function/section chunk and neighboring boundary context;
  an import, heading, enclosing signature, or split boundary can affect several chunks.
  Line insertions can move every later range without requiring new vectors for unchanged inputs.
- Tree-sitter incremental parsing is an optional optimization using retained trees plus a
  validated text-edit diff. Filesystem events generally give paths, not reliable edit ranges.
  Combine actual text differences/input hashes with syntax ranges: same-shape literal edits
  must still invalidate their chunks. Fall back to a full parse of that file whenever uncertain.
- Markdown heading/fence/list changes may alter structure through the end of the file.
  Correct file reparsing is preferable to unsafe line-only invalidation.
- Related-file metadata follows explicit dependency edges; no unconditional repository-wide
  re-embedding. Broader parser/config/profile changes may legitimately require broader rebuilds.

Watch uses a configurable debounce (initial proposal 200 ms), stable-read retries for atomic
saves, and per-path latest-content coalescing. Bulk changes use a bounded queue and reconcile
inventory after branch switches/overflow; ignored outputs cannot cause self-indexing loops.
Persist source/chunk hashes so incremental behavior survives process restarts. Retained parse
trees are disposable acceleration, not required for correctness.

Publish current lexical/symbol coverage promptly while optional embeddings queue separately.
Pending/failed semantic chunks must not serve vectors for replaced text; report vector coverage
and use lexical fallback. Vector jobs publish only if their expected scope/content/config hashes
still match; discard superseded work. Search exposes pending paths and last scan time; strict
freshness performs reconciliation or fails clearly without implicitly calling remote providers.
Expected-hash exact reads remain mandatory even when watch reports healthy.

CLI and library indexing accept affected paths as scheduling hints, but cannot use them to
declare unchecked corpus paths globally fresh. Provide an opt-in MCP refresh operation sharing
the same index service, constrained to configured roots/providers/budgets. Read-only MCP remains
the default; refresh does not execute repository scripts or change code/docs.

Measure save-to-lexical-publication p50/p95, files parsed, chunks rewritten, vectors recomputed,
and burst/backlog behavior. Proposed warm single-file lexical target: p95 <=250 ms after
debounce for files <=100 KiB on declared reference hardware/corpus. Report debounce and provider
latency separately. This is a benchmark target, not a guarantee that every save is trivial.
Incremental and clean rebuilds must produce equivalent evidence/ranking under deterministic
settings. Test line shifts, same-shape edits, boundary changes, rapid saves, deletion/rename,
restart, failed provider jobs, and config/ignore/branch invalidation.

## 8. Retrieval and agent tool contracts

### Search pipeline

Apply repo/snapshot/path/type/lifecycle filters before ranking. Exact path/symbol matches take
precedence for identifier queries. Lexical FTS supports path, symbol, heading, and body fields.
Semantic search complements it for conceptual queries. Fuse independent rankings with reciprocal
rank fusion (initial k=60, configurable), not raw addition of incomparable scores.
Deduplicate repeated/sibling ranges and keep a small diversity of files.
Apply modest, explainable current-doc preference for current-behavior questions; preserve history
for explicit why/history/plan queries. Similarity and authority are separate result fields.
Reranking is optional and disabled initially; add only with measured benefit and budget.

### Proposed MCP methods (also CLI JSON operations)

- `search_project_knowledge`: query, repoId, snapshotId/mode, optional path/role filters,
  maxResults (default 5, cap 20), maxOutputTokens (default 1200), continuation cursor.
- `read_project_knowledge`: opaque result/source ID, expected source hash, requested range or
  bounded parent expansion, maxOutputTokens (default 2500).
- `related_project_knowledge`: source ID, allowed relation kinds, limit and token budget.
- `project_knowledge_status`: indexed revision, scope, dirty state, lexical/vector coverage,
  staleness, last error, profile and limits.

Search response: schemaVersion, effectiveScope, generationId, retrievalMode, coverage, results,
truncated flag, opaque next cursor, and counted/estimated output tokens.
Each result: sourceId, path, revision/hash, role/lifecycle, range, title/symbol, compact excerpt,
ranking explanation, optional validated URL. Do not dump embeddings or full files.
Cursors bind to filters/scope/generation and have expiry; reject reuse on a changed query.

Read returns exact text with provenance, range, available parent/neighbor references, and any
truncation/continuation marker. Opaque IDs are references, not authorization; every operation
rechecks configured repo scope/permissions. Enforce a server output cap in addition to client
budget. Count with the configured tokenizer; label estimates when none is available.
No-query/no-match is a first-class response with lexical/filesystem fallback suggestions.

Stdio MCP is the initial transport. A hosted multi-user service requires a separate auth/ACL
design and is deferred. Repository file/PR text is untrusted evidence: no retrieved content may
change tool grants, execute commands, or become agent system instructions automatically.
Authoritative AGENTS.md loading remains the agent runtime's responsibility, outside RAG.

### Generic agent access and integration boundary

CLI JSON is the universal baseline for agents that can execute permitted terminal commands.
Install the versioned native executable directly or through the chosen distribution wrapper.
For npm consumers, reference the local wrapper via their package manager; native users invoke
the executable without Node. Include both paths, not machine-specific global locations.
AGENTS.md may teach status/search/read/refresh, but cannot grant shell permissions, register MCP,
or guarantee that a client loads instructions. Package config controls indexing, not agent tools.
Client MCP registration is a separate optional step; init can generate reviewed client-specific
examples only after compatibility testing. Rust agents can call the library API; other custom
agents use subprocess JSON or MCP, with optional thin SDKs deferred.

A runner resolves its actual root/worktree and reconciles locally before retrieval; agents use
compact search then exact reads. Missing package/index/tool permission must produce a clear
filesystem-search fallback. Do not require search for trivial known-path edits. Test CLI-only,
MCP, and library clients against equivalent result contracts.

## 9. Git history and PR breadcrumbs

Local history is opt-in and offline-capable. Default history preset uses mainline commits,
a configurable horizon (initial 500 commits), and bounded changed-path summaries.
Record horizon/shallow-clone gaps. Include commit message, parents, changed paths, and optional
bounded diff hunk descriptors; do not embed every patch/full revision by default.
History search is a separate lane or explicit filter, so old code does not pollute current search.

Recognize commit-message PR references as unverified hints. GitHub enrichment optionally calls
read-only APIs to verify commit-to-PR association and fetch title/body/review discussion under
strict limits. Cache ETag/cursors, handle pagination/rate limits/deleted PRs and permission loss.
Do not crawl all issues/PR comments implicitly. Expose provenance, fetched time, edited state,
and remote coverage. PR text is historical intent, not current implementation proof.

Use already configured read-only credentials through a provider adapter; never ask the agent to
read a token file or put tokens in config committed to Git. Remote enrichment is disabled by
default. A missing credential does not stop local indexing. Disabling/removing a repo must
invalidate its served data; remote revocation must fail closed before serving refreshed remote
content. Existing cached private content requires an explicit retention/access policy.

## 10. Configuration and installation contract

Proposed committed root config: `repoise.config.json` with a versioned published JSON Schema
for validation/editor completion. Product prefix is settled; schema versioning remains explicit.
Do not support executable JS/TS config or multiple config formats initially.
Precedence: CLI flags > explicitly selected local overrides > committed config > defaults.
Local overrides remain untracked and cannot bypass mandatory containment/secret policy.
One root config supports named monorepo scopes and per-scope overrides; nested config inheritance
is deferred. Reject unknown keys; doctor exposes redacted effective settings and their origins.
Ignore policy controls indexing, not the agent's independent permission to read files.
Proposed generated data: local user cache keyed by repo scope, or an explicitly configured
ignored directory. Never put index/vector data into source control.

Config groups: repo roots/scopes, include/exclude rules, document-role rules, parser options,
chunk budgets, embedding provider/profile/tokenizer, storage, retrieval limits, optional code,
history horizon, remote enrichment, and retention. Unknown keys and invalid budgets fail doctor.
Secrets resolve only via environment references. Endpoint/model settings are operator-selected;
there is no mandatory cloud provider.

### Proposed CLI (not available today)

`repoise init --dry-run`, `doctor`, `explain <path>`,
`index --mode working-tree [--paths ...]`, `index --ref HEAD`, `watch`, `check --fresh`,
`search --query "..." --json`, `read --source-id ... --json`,
`status --json`, `purge --repo ...`, and `serve --stdio`.

Init produces a reviewable overlay diff and manifest, then applies only explicit chosen files.
It never replaces an existing package.json, AGENTS.md, .gitignore, or MCP config wholesale.
Use a managed, bounded marker block only when the owner opts in; preserve surrounding bytes.
Uninstall removes only unchanged managed files/blocks and reports conflicts.
Update is a three-way template migration with dry-run diff and rollback backup.
Pin tool version and template version separately in the manifest.

The optional owner-installed AGENTS snippet covers:
- Read relevant current docs and scoped agent instructions before changes; distinguish proposals.
- Use exact/lexical lookup for known identifiers, optional semantic search for unknown concepts,
  and bounded exact reads with scope/hash validation.
- Update affected architecture, behavior/config/deployment/testing docs in the same change;
  check links and remove outdated claims. A fresh index is not proof of accurate documentation.
- After edits, verify lexical freshness through status/check. If watch is disabled/unhealthy,
  invoke the incremental index command for affected paths then reconcile before declaring fresh;
  rerun search after stale_result. Use the permitted refresh tool only if explicitly enabled.
- Follow existing project validation/formatting; do not commit generated indexes or enable
  remote embeddings merely to satisfy documentation maintenance.

Init offers this snippet as an opt-in reviewable diff, never as retrieved system instructions.
Document drift checks initially validate links/paths and supported symbol references with
evidence and uncertainty; semantic correctness and automatic prose rewriting remain out of scope.
It also describes token budgets. It complements curated Markdown; it does not require search on every turn.
Provide CLI-only integration for agents without MCP. Verify Cline/Kanban client tool support
before publishing client-specific configuration examples.

### Native executable, npm distribution and initialization

Product/CLI name is Repoise; commands below remain proposed until implemented.
Recheck registry names/scopes and distribution assets before publishing.
Support local devDependency installation for repeatable automation and an explicit-version npx
entry point for evaluation. Document the recommended pinned install; CI must not execute an
unversioned latest package. No install/postinstall hook scans a repository, downloads a model,
contacts a provider, or edits project files.

`init` detects the repository root and presents docs-only, docs+code lexical, and hybrid presets.
Default to offline docs+code lexical. Offer an explicitly selected initial index so init can
produce useful offline search without provider setup. No remote calls/model downloads occur by
default; optional locally installed embeddings preserve offline operation. Before enabling a
remote provider, explain what source/query content leaves the machine and configure budgets.
Interactive use can choose roots/folders, includes/ignores, role mappings, history, provider,
storage, and optional integrations. Noninteractive `init --preset ... --yes` uses documented
defaults, never prompts, and enables no remote features without explicit configuration.
Hybrid setup validates a deliberately selected provider; docs/code lexical works offline.

Commit the versioned config and optional approved scripts; keep generated indexes ignored.
Initialization is idempotent, offers dry-run, preserves existing files, and explains effective
settings through doctor. Migration requires explicit update and review; a package upgrade does
not silently change corpus policy. Config uses portable paths and environment references so it
can run locally and in CI without committing credentials.

Optional package.json scripts, MCP wiring, and workflow examples are separately chosen additions.
Do not assume npm as the consuming project's package manager: honor pnpm/npm/yarn/bun conventions.
The Rust library exports documented configuration, indexing, search, and read contracts; CLI/MCP
use those same APIs. Repository contents are processed as data, never imported/executed.

Build native release artifacts from the same Rust engine, with architecture/platform selection,
checksums and pinned versions. First npm release is a thin launcher with platform-specific
binary packages/assets; prefer registry-delivered artifacts rather than unverified install-time
downloads. No model downloads or indexing during installation. Direct native users can build
with Cargo from day one; reviewed executable archives are the next distribution path.
Homebrew, OS packages, containers and other ecosystem wrappers are future distribution choices,
not separate implementations or current release promises. No silent source compilation fallback
for unsupported npm targets: fail with a clear supported-target/native-build alternative.

Both native and npm installs must run the same offline fixture and produce equivalent results.
Test native invocation with Node/npm absent. Keep release versioning synchronized across wrapper
and engine; migrations depend on engine/schema versions, not installer identity. Embedding runtime
shared libraries/model assets may be separate: do not promise a single fully static executable
without validating dependencies on each target. Installation UX must explain those requirements.

Package release acceptance includes:
- An explicit native target matrix (OS/architecture/libc), MSRV/toolchain, tests on Linux,
  macOS and Windows; separately document the npm wrapper's supported Node versions.
- A packed-tarball smoke test in a clean consumer: init, lexical index/search/read, and stdio MCP.
- Bundled/versioned schema and parser assets resolving from installed package paths, not source cwd.
- SQLite/native/WASM dependencies tested on supported targets with clear unsupported-platform errors.
- Optional parsers/embedding dependencies kept out of the minimum offline installation where feasible.
- Semver for public API/config contracts, separate index/template schema versions, changelog,
  migration/rollback instructions, dependency/license review, and an explicit publication workflow.
- No bundled tokens, private pilot content, generated indexes, or development fixtures containing
  private repository data. Public publication is a separately authorized release action.

### Automatic refresh and build/CI integrations

Use one incremental index operation for manual runs, watch mode, build scripts, and CI.
`watch` is explicitly started, debounces eligible file/config changes, coalesces events,
serializes publication, and rescans after missed events or branch switches. It is not installed
as a system daemon. Readers keep the last complete generation during refresh; status reports
pending changes and errors. SIGINT cancels safely. Dirty worktree scope remains explicit.

A build integration invokes indexing as a separate opt-in script/prebuild step. Default index
failure returns nonzero; any best-effort wrapper must be explicitly configured and visibly report
degraded coverage. `check --fresh` exits nonzero for a missing/stale required generation or unmet
configured coverage without triggering provider calls. Define stable exit-code categories and
machine-readable status for automation. Do not claim a committed index is stale merely because
an excluded build output changed.

Provide a minimal GitHub Actions example that installs the pinned npm package, resolves the
requested checkout SHA, builds or verifies the configured index, and optionally uploads an
artifact. A separately maintained thin Action wrapper is optional after the CLI works; it must
call the same APIs and add no parallel index implementation. Local use does not require GitHub.

CI policy:
- Explicit trigger/ref selection for pushes, pull requests, or scheduled refresh; retain the exact
  checked-out SHA and report shallow-history coverage.
- A clean checkout can rebuild from scratch. Cache is optional acceleration, keyed by scope,
  schema/config/parser/embedding fingerprints; validate manifests before reuse.
- Never share private corpus caches/artifacts with public jobs or unrelated repositories.
  Fork PR jobs receive no remote embedding/history credentials and use lexical mode or skip the
  explicitly credential-dependent step with reported coverage.
- Do not run untrusted PR package scripts with privileged credentials or use pull_request_target
  to execute PR content. Workflow permissions default to read-only contents; indexing does not
  post comments, commit indexes, or publish npm packages.
- CI-produced artifacts include manifest/provenance and explicit retention. A local agent must
  validate scope/fingerprints and exact revision before using an imported artifact; building an
  index in CI alone does not make it available to the agent's local checkout.

Configuration separately selects corpus, refresh triggers, required coverage, remote budgets,
and cache/artifact retention. Document freshness as a declared snapshot plus pending work,
not a promise that a background process always sees the latest source.

### Future deployment needs

Standalone: supported native executable/OS target, writable local cache, SQLite FTS5, and
optional parser grammars. Lexical-only operation needs no Node runtime, GPU or embedding server.
The npm launcher additionally needs its supported Node runtime; native installation does not.
Embeddings require explicitly selected endpoint/model/profile, optional environment credential,
and artifact fingerprint. History needs Git; remote PR enrichment additionally needs read-only
GitHub repository/PR access. No inbound port for stdio. No schema migration to gateway.sqlite.
ai-gateway pilot should add separate opt-in commands/config; never change `pnpm rag:index`.
A hosted capability is a later optional adapter, not a prerequisite for local agents.

## 11. Evaluation and adoption gates

Build frozen synthetic fixtures plus an owner-approved sanitized ai-gateway snapshot.
At least 40 labeled queries across exact identifiers, unknown concepts, current-vs-plan conflicts,
history reasoning, code behavior, tests, unsupported languages, and dirty branch scenarios.
Hold out at least 10 queries during tuning. Include an unrelated docs-only and Python/non-TS repo.
Gold evidence is path/range/revision, not model prose.

Compare the same agent/model/tasks with: rg/filesystem only; curated docs+rg; lexical index;
docs hybrid; docs+code lexical; docs+code hybrid. Keep prompts, context cap, revision, and task
criteria fixed. Record output tokens from search/read, model input/output, tool count, elapsed
time, and task correctness. No claim of efficiency merely because search results are small.

Initial acceptance targets (proposed gates, to be reviewed before implementation):
- 100% exact path/symbol fixture lookup and valid source/range provenance.
- Zero cross-scope/stale-text/secret-policy fixture leaks.
- At least 85% evidence recall@5 on held-out eligible conceptual queries.
- At least 20% median reduction in total model input tokens versus curated docs+rg, with no
  decrease in correctly completed replay tasks. Report sample size and uncertainty.
- Code vectors ship enabled in the pilot only if they improve code-query recall@5 by at least
  10 percentage points over code lexical at an acceptable measured build/memory cost.
- p95 warm search <=500 ms lexical and <=2 s hybrid, excluding a separately reported remote
  embedding request. Publish hardware, file/chunk counts, memory, model, and cold-build times.

These are targets, not measured results. If gates fail, keep lexical/docs mode and change the
design; do not manipulate the benchmark to justify vectors. Reranking/ANN get separate ablations.

### Early public-repository benchmark matrix

Run an initial engineering benchmark immediately after K2 offline CLI behavior works; add code
runs after K4, and semantic modes after K3. Do not wait for the final integrations card.
Select pinned public commits covering docs-heavy, TypeScript, Python, and large multilingual
monorepo workloads. Candidate corpora include TypeScript, VS Code, CPython and Kubernetes;
these are candidates, not executed benchmarks or commitments to index every file.
Record source license, commit, corpus rules, eligible bytes/files/chunks, language coverage,
hardware/OS/runtime, versions and effective settings. Total checkout size is not index size.
Supplement with synthetic saves/bursts/large files/branch changes and immutable reference outputs.

Report:
- Cold/full runs: discovery, parse, lexical publication, model acquisition/loading, inference,
  vector publication, total wall time, peak RAM/CPU and temporary peak disk use.
- Steady storage: source/chunk text, metadata/edges, FTS, vectors, model artifacts, retained
  generations, cache/WAL and reclamation overhead, separately and in total.
- Updates: no-op, one literal/function/paragraph, line insertion, rename/delete, imports/headings,
  bulk formatting, branch switch and ignored/config changes. Measure p50/p95 save-to-searchable
  latency, debounce separately, parsed files, rewritten chunks and actual embedding calls.
- User benefit: repeat identical agent/model/tasks/revisions/budgets with rg only, curated docs+rg,
  lexical and optional hybrid. Count correct tasks, regression/test results, total model input/
  output tokens, tool calls, elapsed time, retrieval errors and stale evidence.
- Distinguish first-task setup cost, warm reuse, and amortized multi-task cost. Report repeated-run
  variability/sample size. Freeze task sets and hold out tuning queries; faster retrieval alone
  is not proof of a better coding agent.

### Size controls and compression experiments

Budget by eligible chunks, not total Git size. Raw vector bytes approximately equal chunk count
times dimensions times bytes/component: 1,000,000 x 768 x 4 is 3.072 GB (decimal), vectors alone.
FTS/text/edges/models/history/temporary publication add overhead; benchmark rather than predict
database size from this formula. Bounded cosine scan remains for small indexes only; larger
corpora require measured capacity limits, partitioned search or a separately validated ANN adapter.

Apply low-risk controls first: omit generated/vendor content, disable history/vectors where
unneeded, reduce redundant overlap, deduplicate identical safe input, cap retained snapshots,
and reclaim obsolete generations. Never evict an active generation or its referenced vectors.
Expose estimate/status byte breakdown, dry-run eligible chunk estimates, maxIndexBytes,
maxCacheBytes, retention and minimum free-disk headroom. Model artifacts have a separate budget.
If capacity is exceeded, stop publication or explicitly report configured partial coverage;
never silently skip evidence and claim a complete index. Verify headroom for temp/WAL files.
Document lexical-only low-resource and balanced presets; do not present them as fixed size promises.

Lossless compression is appropriate for exported artifacts and optional cached source payloads,
not blindly gzipping a database needed for random access. FTS postings and hashes still occupy
space. Avoid duplicated full-file text when chunk payloads/provenance suffice; committed exact
reads can use Git blobs, while dirty reads validate the filesystem hash. Do not retain unlimited
source snapshots or promise pinned dirty reads after their content changes.

Benchmark optional fp16 or int8 vector storage against float32. Theoretical component storage is
2x/4x smaller respectively, excluding scale metadata and any retained full-precision copy.
If originals remain, RAM may improve without comparable disk savings. Binary/product compression
is deferred until measured recall justifies complexity. Distinguish stored-vector quantization
from embedding-model weight quantization: the latter reduces model footprint/inference cost,
not necessarily generated vector storage. Reduced dimensions require a model-supported method
or validated projection and matching query profile; arbitrary truncation is not acceptable.

Changing overlap/chunk size/model/dimensions/precision can affect recall and incremental stability.
Evaluate storage/RAM/build latency/search latency alongside evidence recall and task correctness;
no preset silently trades correctness for smaller output. Compression is optional optimization,
not a blocker for the first offline CLI milestone.

### Optional built-in local embeddings

Semantic dense embeddings require model inference for changed chunk inputs and semantic queries;
lexical/symbol retrieval does not. The user need not operate an embedding server: evaluate an
optional Rust local-inference adapter (evaluate ONNX Runtime bindings or a compatible Rust
runtime) with a pinned small compatible
sentence-embedding model, correct pooling/normalization/query prefixes/tokenizer and licensed assets.
CPU is the baseline; GPU acceleration is optional. Benchmark prose and code separately rather
than assume a small text model is a good code model.

Provide zero-provider-configuration as an explicit local preset: package selects a tested default
artifact, exposes expected download/disk/RAM cost, and acquires it only on an explicitly requested
setup command. A first download is network-dependent even though all subsequent inference is local.
Support pre-provisioned model directories and strict offline mode that disables remote fetches.
Missing assets produce actionable lexical fallback. No hidden npm postinstall/first-search
downloads or unrequested telemetry. Index and query use identical pinned model/profile fingerprints.
Optional dependencies and model caching must preserve a small lexical installation and avoid
repeated downloads per consuming repository. Shared model artifacts contain no private corpus;
source/vector caches remain scoped.

Model choice, dimensions/weight precision, runtime versions, platform support and release license
are evaluated during K3, not promised by this plan. Include local model bytes and query cold-start
time in benchmarks. A built-in model removes provider setup, not inference/storage cost.

### Existing-tool assessment before implementation

This is an established problem, not a novel category. Compare maintained versions of adjacent
tools before committing to a new core:
- [crumbs-cli](https://pypi.org/project/crumbs-cli/): compact local file/symbol maps, term-ranked
  search and CLI/MCP; Python AST and regex extraction for other languages. Compare this lightweight
  baseline before assuming body embeddings or a larger index improve task outcomes.
- [Repowise](https://docs.repowise.dev/getting-started/setup): repository context/indexing,
  documentation and agent integration; a related product and close naming neighbor.
- emCP: local SQLite hybrid retrieval, incremental embeddings, CLI and MCP.
- semantic-code-mcp: AST-aware chunking, local ONNX embeddings, hybrid retrieval and file watching.
- Sourcegraph/Cody and Copilot repository indexing: examples of integrated repository context,
  with different client/hosting boundaries from this standalone package.

Project documentation claims establish candidates, not verified performance or implementation correctness.
The central distinction to test is compact interface navigation versus searchable implementation
and document evidence. Do not justify a larger engine merely by Rust or feature count.
K1 records build/adopt/extend reasons, licenses, maintenance/platform fit, and a reproducible small
trial. Focus differentiation on offline lexical onboarding, documentation authority/lifecycle,
compact exact reads, worktree freshness, explicit budgets, and generic CLI/library/MCP integration.
Do not copy code without license review or adopt unsupported speed/token claims.
Include at least one comparable tool in later benchmark runs where setup/contracts permit.

## 12. Implementation cards / reviewable PRs

Each card points here plus only its relevant sections and fixture contracts. Target 6 core PRs
in this repository, plus a separate ai-gateway consumer integration PR;
split a card only if implementation/test scope exceeds the local agent's practical context.

| Card | Scope | Depends on | Acceptance |
| --- | --- | --- | --- |
| K1 | Rust workspace/core/native CLI, thin npm scaffold, neutral adapter/provenance contracts, config/init/discovery/doctor | None | Native run without Node, Git/filesystem/fake-revision contracts, nested ignores, non-GitHub remotes, deterministic manifests, init idempotence |
| K2 | Offline prose index/search/exact read, persistent SQLite/FTS generations, incremental chunk reconciliation, freshness checks | K1 | First offline install/init/index/search/read milestone, structural Markdown, line-shift reuse, restart persistence, atomic rebuild equivalence |
| K3 | Embedding adapters/cache, compatible profiles, incremental hybrid search and budgets | K2 | No re-embed unchanged input, dimensions validated, lexical degradation, token caps |
| K4 | Optional code grammars, symbols, structural chunks and related references | K2; K3 for vectors | TS/JS + fallback fixtures, parser errors, scope parents, code ablation |
| K5 | Bounded local history and optional verified GitHub PR enrichment | K2 | Shallow/horizon reporting, PR verification, pagination/rate-limit/permission tests |
| K6 | MCP/read and opt-in refresh, watch, overlay lifecycle, build/Actions examples, package release validation, evaluation/runbooks; separate ai-gateway consumer PR | K2; K3/K4/K5 for respective optional coverage | Shared APIs, refresh/branch-switch fixtures, safe CI/cache examples, packed install/platform smoke tests, non-destructive lifecycle, measured gates |

K1 must establish interface fixtures so K4/K5 can be implemented without rewriting K2.
K1/K2 deliver the first offline docs milestone without vectors/history/MCP/watch. K4 adds code
symbol/lexical coverage independently of K3; release the recommended docs+code lexical preset
when K4 passes. K6 depends on K2 for offline integration, and only on K3/K4/K5 for the respective
optional coverage. No offline release is blocked by vectors or remote enrichment. Implemented docs and per-card execution
records live in the standalone repo; ai-gateway contains only its pilot integration and plan link.

### Card-level verification

- K1: hostile paths, odd filenames, non-Git roots, shallow Git, unsupported encoding, secret-like
  files, symlinks, ignored/generated data, and root scope collisions.
- K2: fenced headings, long tables, stable ranges, empty files, source hash mismatch, interrupted
  publication, concurrent reader, deletion/rename, and rebuild from scratch equivalence.
- K3: provider timeout/partial batch, cancellation/retry cap, profile/model changes, NaN/wrong-size
  vectors, lexical-only search, deterministic ties, cursor/filter binding and output truncation.
- K4: overloaded/nested/large symbols, imports/comments/decorators, invalid code, unsupported
  grammars, test relationships, and honest uncertainty of reference edges.
- K5: ambiguous PR hints, verified association, remote denial/rate limits, private cache policy,
  edited/deleted comments, missing commit history, and local operation without credentials.
- K6: equivalent CLI/MCP results, scope authorization, injection-as-data fixtures, installer
  idempotence/conflicts/rollback/uninstall, watch debounce/branch-switch/cancellation, freshness
  exit codes, ignored-file config changes, CI cache/artifact isolation, clean packed installation,
  supported-platform and client smoke tests, benchmark gates and real pilot.

Every PR states changed features, config/deployment actions, tests/results, rollback, and
documentation updates. No arbitrary production endpoint/GPU access is needed for contract tests.

## 13. Rollout and rollback

1. Scaffold the Rust workspace in billy-the-ape/repoise; implement and validate the
   Rust executable's lexical/docs mode against fixtures and a copied/sanitized pilot snapshot;
   validate npm distribution against the same contracts, not a separate Node implementation.
2. Validate offline code lexical and explicit watch/startup refresh; enable optional local docs hybrid search for a limited set of Cline/Kanban cards; collect token/task data.
3. Trial code/history independently; promote each only after its gate passes.
4. Install a pinned package and reviewed config/optional overlay in ai-gateway in its own PR.
   Trial build/watch/CI refresh independently; publish publicly only after release validation
   and owner authorization.
5. Consider a gateway read-only adapter or additional repos only after local operation is stable.

Rollback disables the tool/overlay integration and returns agents to curated docs+rg.
Retain previous complete index generation when a new build fails; schema upgrades build into
a new namespace rather than mutating the only working index. Generated cache is disposable.
No device/service changes are authorized by this plan; if implementation changes deployment
devices later, include a final homelab-documentation PR to record those changes.

## 14. Decisions to settle during K1/K6 review

The design defaults above let implementation start without choosing everything now.
Settled: Repoise name, billy-the-ape/repoise repository, Rust core and npm-first native distribution.
Remaining owner decisions: registry package/crate names or scopes, license, public release timing,
approved pilot corpus, actual embedding artifact,
target client versions, maximum local cache/storage cost, and optional remote history scope.
Implementation decisions: Rust MSRV/toolchain and crate versions, native target/libc matrix,
SQLite binding/build options, parser/grammar distribution and local inference runtime/assets,
validated tokenizer per provider, dependency licenses, and platform support matrix.
Record chosen versions and benchmark evidence before distributing the overlay.

### Documentation ownership in this repository

Keep this plan as the v1 scope and decision record. K1 creates a documentation map as actual
subsystems emerge; implementation PRs add concise current-state architecture, development,
testing, storage, agent-integration and distribution guides only when their behavior exists.
Plans and execution records stay distinct from those guides. Link each implementation card
to this master plan plus its relevant sections, acceptance criteria and actual source entry points.
Record shipped/deferred decisions and evidence; do not mark behavior implemented merely because
a plan PR merged. Update AGENTS.md navigation when authoritative guides or validated commands
appear, keeping it small and using scoped instructions only for genuine subtree differences.

## 15. Technical references

These support component capabilities, not a claim that RAG outperforms filesystem search:

- [SQLite WAL](https://www.sqlite.org/wal.html): local persistent concurrent reader/writer
  operation and backup/sidecar constraints.
- [Tree-sitter advanced parsing](https://github.com/tree-sitter/tree-sitter/blob/master/docs/src/using-parsers/3-advanced-parsing.md):
  edit-aware tree reuse; incremental parsing does not replace chunk input invalidation.
- [SQLite FTS5](https://www.sqlite.org/fts5.html): full-text indexing and BM25 ranking.
  Its BM25 scores sort lower for better matches; convert to rank before fusion.
- [Tree-sitter query syntax](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax.html):
  structural captures and error nodes; symbol extraction still requires language-specific queries.
- [Tree-sitter](https://github.com/tree-sitter/tree-sitter): syntax parsing, not semantic type resolution.

- [Tree-sitter Rust bindings](https://docs.rs/tree-sitter/latest/tree_sitter/) and
  [ort](https://github.com/pykeio/ort): candidate Rust parser/ONNX integration components;
  versions, licenses, native assets and platform support must be validated.
- [Qdrant quantization](https://qdrant.tech/documentation/manage-data/quantization/):
  vector compression tradeoffs, not a requirement to use Qdrant.
- [emCP](https://github.com/eeveere/emCP) and
  [semantic-code-mcp](https://github.com/smallthinkingmachines/semantic-code-mcp):
  comparable repository retrieval projects requiring implementation/license assessment.
- [Sourcegraph Cody](https://sourcegraph.com/docs/cody) and
  [Copilot repository indexing](https://docs.github.com/en/copilot/concepts/context/repository-indexing):
  integrated context retrieval precedents.

All model/framework/storage choices remain subject to fixture tests and evaluation.
