# 0.1.0-alpha.1 publication record

Published October 15, 2026 (America/Denver). This is a functional preview,
not stable v1. Do not replay completed staging or publication commands.

## Source and build evidence

- Tag: v0.1.0-alpha.1; source commit:
  d44e124d949b2c87eb6577860721867d66601b3c.
- [Release preparation PR #21](https://github.com/billy-the-ape/repoise/pull/21)
  merged after CI passed.
- [Artifact-only run 38814756888](https://github.com/billy-the-ape/repoise/actions/runs/38814756888)
  passed tag checks, Linux/macOS/Windows builds and functional packaged-consumer
  smokes. Metadata, exact npm versions and all seven payload SHA-256 checksums
  were independently checked.
- [Publishing run 38817743343](https://github.com/billy-the-ape/repoise/actions/runs/38817743343)
  rebuilt the same source, passed all builds/smokes and created the native draft.
  The maintainer downloaded this run's release-bundle, confirmed the source SHA,
  version and next tag, and verified all seven SHA-256 checksums before local staging.

## npm publication

All four packages were staged locally with npm 11.21.0 from the verified
publishing-run bundle, then approved with maintainer authentication.
Native packages were published/verified before launcher approval.

| Package | Stage ID | Published tarball SHA-1 |
| --- | --- | --- |
| repoise-linux-x64-gnu | 57b255d8-5f87-4c9d-b4be-e87a5483d953 | 4ddb927046895c9debb94f279f236c62542ff3b3 |
| repoise-darwin-arm64 | b3f1d0e1-3bab-4786-955c-2559a15b1da8 | da451e8d3261c5a12db970bf8f6111fa1d0f142e |
| repoise-win32-x64 | 5b4c9320-43c5-431b-ac84-aa9ebfb49d97 | c0bcddac6e4727930440723ac3cbb84b343dfd67 |
| repoise | fd2ef127-388e-47ad-bb33-7ba9aad060d1 | a8e66fe14903e798cddca8cb7e11a35736c4afff |

Registry metadata supplied by the maintainer matched each tarball SHA-1.
At initial publication, all next tags resolved to 0.1.0-alpha.1 and the
launcher's three optional native dependencies were pinned exactly to that
version. latest was initially left at 0.0.0-stage for Linux/macOS/launcher
and 0.0.1-security for Windows. The October 19 promotion below supersedes
that initial default-install state.

Windows approval initially returned E409 while npm automated review was still
running; retrying the same stage after review completed succeeded. No restaging
or version change was needed. npm support's name clearance is confirmed by
successful Windows publication.

## October 19 default-version promotion

The maintainer used authenticated npm dist-tag operations to move latest to
0.1.0-alpha.1 for all three native packages, then the launcher. next remains
0.1.0-alpha.1. No package bytes, versions or Git tags were republished or changed.
This makes the alpha preview the default npm install; it is not a stable v1
release and the native release remains marked as a prerelease.

Registry metadata for the launcher's latest version was independently checked:

- repository: git+https://github.com/billy-the-ape/repoise.git
  (directory npm/repoise)
- homepage: https://github.com/billy-the-ape/repoise#readme
- issues: https://github.com/billy-the-ape/repoise/issues

The prior placeholder default lacked these links; the functional version
already carried them, so changing latest required no metadata republish.
npm website rendering may lag registry metadata.

A fresh macOS directory installed with npm install --ignore-scripts repoise,
without an explicit version. Installation added the wrapper and matching native
package, and the installed local executable reported repoise 0.1.0-alpha.1.
This complements the original pinned CLI/MCP tests below; Linux/Windows fresh
registry-install testing remains unmeasured.

## Native publication

The [public native prerelease](https://github.com/billy-the-ape/repoise/releases/tag/v0.1.0-alpha.1)
was verified as draft=false and prerelease=true with the reviewed source commit.
It contains all five expected assets: three platform archives, SHA256SUMS and
release.json. It was not designated a stable release.

## Public-registry consumer verification

Fresh installation on the maintainer's macOS ARM64 system used
npm install --save-exact --ignore-scripts repoise@0.1.0-alpha.1 in a new
temporary directory. The installed wrapper selected
repoise-darwin-arm64@0.1.0-alpha.1.

Verified using the installed local executable, not the source build:

- Version and complete CLI help.
- Init with docs-code-lexical and managed AGENTS.md guidance.
- Indexing a disposable Markdown/TypeScript fixture.
- Freshness check: Ok, fresh=true, coverage met=true.
- Code search for sumLedger and exact read equal to the fixture source.
- Stdio MCP initialization/version, discovery of exactly four default tools,
  search, exact read, refresh refusal and clean shutdown on stdin EOF.

Linux/Windows passed offline packaged-consumer CI smoke. Fresh public-registry
functional installs on those operating systems remain unmeasured.

## Bootstrap cleanup

The maintainer verified that stage 45192162-f09a-4790-9fa7-4be3db7e0060
was repoise@0.0.0-bootstrap.0, rejected it, and confirmed that a subsequent
repoise stage listing returned []. The nonfunctional bootstrap was never
approved. The published 0.1.0-alpha.1 preview remains unchanged.

## Authentication and remaining work

OIDC staging failed on the first Linux package with ENEEDAUTH. The maintainer's
trust listing matched release.yml, billy-the-ape/repoise, npm-release and
createStagedPackage; stage listing was empty. The pinned npm client suppresses
OIDC exchange failure details at normal log level; the underlying cause remains
unresolved. Local staged publishing succeeded. This release has no CI-generated
npm provenance; checksums and source metadata are integrity evidence, not signing.
The two-dispatch sequence above was the alpha.1 fallback for local staging, not
a workflow requirement; the runbook documents a single-dispatch sequence.

- Diagnose OIDC failure without adding registry bypass tokens.
- Verify public-registry functional installs on Linux and Windows.
- Continue identifier-ranking and agent-workflow investigations separately.
  Initial trials do not establish consistent speed/token gains.
- Hybrid gains and real-agent adoption gates remain unproven.

The published tag and package bytes must not change. Documentation corrections
land on main through a separate PR; fixes to executable behavior need a new
prerelease version. Cargo/crates.io publication remains disabled.
