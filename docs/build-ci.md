# Build and CI integration (consumer guide)

Current state: offline docs+code **lexical** indexing is integrated as a separate,
explicitly-invoked step. There are no automatic build hooks, no installed daemon and no
remote embedding or history provider in this path. This guide covers the consumer
integration (pinned install, reviewed config, generated-cache ignore policy, opt-in
scripts, `index` then `check --fresh`), a minimal GitHub Actions example, and the cache
and private-corpus isolation rules a future cache or artifact import must satisfy. It is
implementation of the build/Actions examples only: it does not mean the live agent pilot
or the master-plan section 11 adoption gates have passed.

## Decisions for this integration

- Offline `docs-code-lexical` preset (embeddings and the history lane stay opt-in).
- Indexing is a separate, explicitly invoked command; no git hooks, no daemon.
- In CI the index rebuilds from scratch; no cache restore and no artifact upload here.
- An index failure stops the build; there is no silent best-effort fallback.
- Read-only workflow permissions; no credentials and no remote providers.

## Pinned installation

The functional 0.1.0-alpha.1 preview is published under both npm `latest` and
`next`. Default installation selects this alpha; pin it explicitly for repeatable
CI. Do not use the scaffold 0.1.0-alpha.0.

    npm install --ignore-scripts --no-audit --no-fund \
      repoise@0.1.0-alpha.1

The example workflow retains its clearly labeled version placeholder; replace
it with this exact preview pin when evaluating the integration. Fresh registry
installation is verified on macOS ARM64; Linux/Windows are covered by offline
packaged smoke but not yet fresh public-registry installation.

In CI the launcher installs into a dedicated tools dir under `runner.temp` (outside the
checkout) and is invoked by absolute path with Node — see the example below.

The native engine needs no Node/npm;
npm is the first distribution channel. For source builds, use local
development installation (`bash scripts/install.sh`) or the built native binary
(`./target/release/repoise`); see [development.md](development.md).

## Reviewed configuration

Configuration is committed, reviewed data (never executed) and follows
`schemas/repoise.config.v1.schema.json` (schema version 1). Generate it
non-interactively, then review the effective settings before indexing:

    repoise init --preset docs-code-lexical
    repoise doctor                    # effective settings, capabilities, policy
    repoise explain --path .env       # include/exclude decision for one path

The committed config you review selects the corpus preset and the generated-cache
location (both are shown explicitly here even where they are the defaults):

    {
      "schemaVersion": 1,
      "preset": "docs-code-lexical",
      "cacheDir": ".repoise"
    }

## Generated-cache ignore policy

The cache root is generated data only and must stay out of version control:

- The default root is project-local `.repoise/`; `cacheDir` or the `REPOISE_CACHE_DIR`
  environment variable relocate it. Never point the cache into version control.
- Keep `.repoise/` and the operator-only override `repoise.local.json` ignored.
- The cache is disposable: `repoise purge` removes one scope, `repoise purge --all`
  removes the whole layout; source files are never touched.

Add to the consumer's ignore file:

    .repoise/
    repoise.local.json

## Opt-in package scripts

Optionally expose the commands as package scripts on an **owned** checkout. These are
convenience wrappers a maintainer invokes on purpose; they are not hooks and do not run
automatically, and the CI example below deliberately does not run them.

    {
      "scripts": {
        "repoise:init": "repoise init --preset docs-code-lexical",
        "repoise:index": "repoise index",
        "repoise:check": "repoise check --fresh",
        "repoise:watch": "repoise watch"
      }
    }

## Index, then check --fresh

Indexing and the freshness gate are two separate, explicitly invoked commands. A
successful index must be followed by `check --fresh` before the build continues:

    repoise index          # build and publish the offline index (incremental)
    repoise check --fresh  # stable exit codes: 0 ok, 1 error, 3 missing/stale, 4 coverage not met

`check --fresh` makes a missing or stale required generation, or unmet configured
coverage, a nonzero exit without triggering any provider call. Wire it so a nonzero exit
stops the build; do not wrap it in a best-effort fallback.

## Startup refresh vs. a separately started watch

Freshness is a declared snapshot plus pending work, not a promise that a background
process always sees the latest source.

- **Startup refresh (agent checkout).** At session start, the agent reconciles its
  actual checkout with one explicit `repoise index`, then may start `watch`. Unchanged
  content reuses persisted work; changed content republishes. This refreshes the
  agent's **local** `.repoise` cache.
- **`watch` (separately started).** `repoise watch` is an explicitly started loop that
  debounces eligible file/config changes, coalesces per-path content, publishes at most
  one generation per step and rescans after branch switches. It is not installed as a
  system daemon and cancels safely on SIGINT or SIGTERM (machine consumers can follow
  the run through the optional `--events` NDJSON stream; see the
  [automation guide](automation.md)). If `watch` is unavailable or unhealthy,
  invoke the incremental `repoise index` for the affected paths and reconcile before
  declaring fresh. When embedding is configured, each watch publication runs the
  same cache-first embedding pass as `repoise index`: changed/new chunk inputs are
  embedded and unchanged inputs are served from the content-addressed vector
  cache. A provider failure or cancellation leaves the published generation
  lexically complete with partial or no vector coverage; `check` reports unmet
  coverage until an explicit `repoise index` restores vectors (a later watch
  publication re-attempts the failed or pending inputs when it runs).

See the [MCP guide](mcp.md) for the opt-in refresh tool and [storage.md](storage.md)
for the generation/freshness model.

## Build integration (separate opt-in step)

A build integration invokes indexing as a separate opt-in script or prebuild step:

- Default: a `repoise index` failure returns nonzero and stops the build.
- Any best-effort wrapper must be explicitly configured and visibly report degraded
  coverage; this example does not include one.

## Minimal GitHub Actions example

[`examples/ci/repoise-ci.example.yml`](../examples/ci/repoise-ci.example.yml) is a
minimal, copy-paste-ready example. It is an example, not an active workflow: it does not
live in `.github/workflows/` and is not executed by this repository's CI. It:

- Triggers on `push` and `pull_request` — never `pull_request_target`, so untrusted PR
  content is not executed.
- Checks out with `persist-credentials: false`, so no checkout token is available to
  later steps; the index needs no remote access.
- Uses read-only `contents: read` permissions; indexing neither posts comments, commits
  indexes nor publishes npm packages.
- Installs the pinned launcher (placeholder version, `--ignore-scripts`) into a
  dedicated tools dir under `runner.temp` — never into the consumer checkout — so the
  checkout's `package.json`, lockfile and `node_modules` are never read or modified.
- Invokes the installed launcher explicitly with Node and its absolute path (no
  `npx`), passing `GITHUB_WORKSPACE` as the repository root; it does not run any
  consumer `npm run` scripts.
- Verifies that `git rev-parse HEAD` equals `GITHUB_SHA` (the actual checked-out
  revision), then rebuilds the index from scratch into a per-run cache that is separate
  from both the tools dir and the checkout (`REPOISE_CACHE_DIR`); no cache restore and
  no artifact upload.
- Uses the committed, reviewed consumer config and selects `docs-code-lexical`
  explicitly (`--preset docs-code-lexical`) for both `index` and `check`. Optional
  embedding providers and history enrichment stay **disabled** for this offline
  example.
- Fails the run when `repoise check --fresh` is nonzero, then confirms no tracked
  consumer files changed after install and indexing.

A default shallow checkout (depth 1) means the optional history lane, if enabled, is
bounded to the available commits and reports explicit gaps rather than inventing
history. In this offline example the history lane is off, so there is no remote or
history coverage to report.

## A CI index does not refresh a local checkout

Building an index in CI only populates CI's own cache root for that run. It does not
make the index available to, or refresh, an agent's local checkout: the agent's local
`.repoise` is a separate scope. An agent must run its own `repoise index` (startup
refresh) or `watch` against its real checkout, as above.

Future cache/artifact import (not implemented in this example) must satisfy the
following before a local agent reuses a CI-produced cache or artifact:

- Match scope (repository root/snapshot) and the exact revision; otherwise reject.
- Validate the schema/config/parser/embedding fingerprints in the artifact manifest and
  rebuild locally when they are incompatible.
- Validate manifests before reuse; never trust an artifact that lacks provenance.
- Never share a private corpus cache or artifact with public jobs or unrelated
  repositories; fork PR jobs receive no remote embedding or history credentials and run
  lexical-only or skip the credential-dependent step with reported coverage.

This is documentation of requirements only; no cache restore, artifact upload or import
machinery is implemented here. See [storage.md](storage.md) for the scope/fingerprint
layout.

## Validation status

- **Local validation (performed).** The corrected CI pattern was run in a disposable
  Git consumer (committed reviewed config, `package.json`, `package-lock.json`, one
  Markdown doc and one TypeScript source) against the current packed artifacts,
  offline. The launcher + native package installed into a dedicated tools dir outside
  the checkout (`--ignore-scripts`), the launcher was invoked explicitly with Node by
  absolute path (no `npx`), and the checkout was passed as the repository root with an
  isolated `REPOISE_CACHE_DIR` separate from both the tools dir and the checkout.
  `git rev-parse HEAD` matched `GITHUB_SHA`; `index` then `check --fresh` (both with
  `--preset docs-code-lexical`) exited `0`; and `git status --porcelain` stayed clean,
  with `package.json`, `package-lock.json` and the config byte-identical. The recorded
  run is in the PR description.
- **GitHub workflow execution (not performed).** `examples/ci/repoise-ci.example.yml`
  is an example only; it was not executed against GitHub Actions and no workflow is
  activated by this change. Its registry-install line uses a placeholder version and is
  not a validated install path.
