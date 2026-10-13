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

No functional release is published yet. Registry-install examples therefore use an
explicitly labeled placeholder and are **not** a validated install path:

    # Substitute REPLACE_WITH_VALIDATED_RELEASE_VERSION with a published version.
    # Do NOT use 0.1.0-alpha.0 (the scaffold alpha): it predates the engine and
    # provides greeting/help/version only.
    npm install --ignore-scripts --no-audit --no-fund \
      repoise@REPLACE_WITH_VALIDATED_RELEASE_VERSION

The publication runbook supplies the concrete pin. The native engine needs no Node/npm;
npm is the first distribution channel. Before a functional release, use local
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
  system daemon and cancels safely on SIGINT. If `watch` is unavailable or unhealthy,
  invoke the incremental `repoise index` for the affected paths and reconcile before
  declaring fresh.

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
- Installs the pinned npm launcher (placeholder version) and invokes `npx repoise`
  directly; it does not run any consumer `npm run` scripts.
- Reports the exact checked-out SHA (`${GITHUB_SHA}`) and rebuilds the index from
  scratch into a per-run isolated cache (`REPOISE_CACHE_DIR`); no cache restore and no
  artifact upload.
- Fails the run when `repoise check --fresh` is nonzero.

The checked-out revision is reported as the checkout SHA string, not re-resolved. A
default shallow checkout (depth 1) means the optional history lane, if enabled, is
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

- **Local validation (performed).** The documented command sequence was run in a
  disposable synthetic consumer against the current packed package — the installed npm
  launcher (from the packed wrapper) and the native binary packed into the native
  archive (no Node on PATH) — offline, with an isolated per-run cache
  (`REPOISE_CACHE_DIR`). For both forms: `init` wrote the reviewed config,
  `check --fresh` on a not-yet-indexed checkout exited `3` (missing index), `index`
  published a fresh generation, `check --fresh` then exited `0`, `status` reported
  `fresh`, and offline lexical search returned the expected doc and code hits —
  confirming the failure-stop semantics. Full native-archive *extraction* is covered by
  the CI packed-consumer smoke (a sandboxed host can restrict nested-file tar extraction).
  The recorded run is in the PR description.
- **GitHub workflow execution (not performed).** `examples/ci/repoise-ci.example.yml`
  is an example only; it was not executed against GitHub Actions and no workflow is
  activated by this change. Its registry-install line uses a placeholder version and is
  not a validated install path.