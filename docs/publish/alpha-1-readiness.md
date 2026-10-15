# 0.1.0-alpha.1 preview readiness

Status: candidate preparation only; not tagged, staged, or published by this PR.
This is the first functional engine preview, not stable v1.

## Included

Offline docs/code lexical indexing, incremental publication, exact source reads
with hash validation, structural/section links, freshness/coverage checks, watch,
managed AGENTS.md guidance (template 1.2.0), and read-only-by-default stdio MCP.
Embeddings and bounded Git history/enrichment are opt-in.

Native targets: Linux x64 GNU, macOS ARM64, Windows x64 MSVC.
The npm launcher requires Node >=22.14.0; native archives need no Node.
Other OS/architecture/libc combinations are not supported by these packages.
Cargo crates remain publish=false; this release does not publish to crates.io.

## Evidence and limitations

- The offline synthetic harness covers 60 labeled queries, provenance,
  policy/isolation and freshness checks. G3 recall uses held-out conceptual
  queries; other gates use their own evidence sets.
- A source-built Mac pilot on a pinned consumer snapshot verified indexing,
  focused search, exact reads, watch and stdio MCP operation.
- Exploratory agent trials do not establish consistent speed/token savings.
  Model query/filter mistakes and premature filesystem fallback remain;
  unfiltered exact-identifier searches may rank docs/fixtures above definitions.
- Strict lexical matching can return zero for broad natural-language queries.
  Refine with shorter symbols and appropriate path/role filters.
- Hybrid gains and real-agent adoption gates remain unproven.
- Source-build pilot evidence does not substitute for candidate artifact smoke
  or fresh registry-install verification.

## Release sequence

Follow [npm.md](npm.md) and [native.md](native.md), using the reviewed
0.1.0-alpha.1 commit. Never reuse or move v0.1.0-alpha.0.

1. Merge the version/readiness PR after CI and review.
2. Create annotated v0.1.0-alpha.1 at the verified merged commit.
3. Dispatch release.yml from main with both publication options false.
   Verify all platform checks, packed-consumer smokes, bundle source SHA,
   version and seven payload checksums.
4. Verify all four npm trusted publishers and Windows publishing access.
   Support cleared the Windows name block; its 0.0.1-security placeholder
   is not a functional binary release. Do not approve the main bootstrap.
5. Stage the same reviewed tag with stage_npm; create a native draft with
   draft_native if desired. Inspect the actual publishing run's bundle;
   separate builds need not be byte-identical.
6. Approve/verify all three native npm packages before approving the launcher.
   Prerelease npm dist-tag is next; do not change latest.
7. Verify native draft assets and publish as a prerelease, not latest stable.
8. Test a fresh pinned registry install and record actual results/platforms.
   Update current installation guidance only after publication is verified.

No runtime secrets are needed for lexical operation. Do not add registry tokens
to the repository. npm login/2FA and protected-environment approvals remain on
the maintainer's account. OIDC staging is not yet proven end to end.

## Consumer verification after publication

In a disposable directory, install repoise@0.1.0-alpha.1 with install scripts
disabled, then verify version/help, init/index/search/exact read/status/check
and MCP discovery/search/read. Use the same functional fixture as the release
smoke; report registry-install evidence separately from offline tarball smoke.
Verify invalid arguments fail and refresh is refused by default.

Existing managed guidance can be previewed with overlay update --dry-run
before applying overlay update. The query-time reliability fixes alone require
no index rebuild; normal freshness checks still apply after source/config edits.

## Rollback

Pin a previously verified functional version, if one exists. The scaffold alpha
is not a functional fallback. Otherwise stop using the preview and retain the
source-build workflow. Do not overwrite published versions or move release tags;
publish a new corrected prerelease. Generated caches are disposable, but only
purge Repoise-generated data. Source code is never rollback cache data.
