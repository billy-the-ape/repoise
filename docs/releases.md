# Releases and npm ownership

## Current state

Repoise is MIT licensed. The first usable distribution version is `0.1.0-alpha.0`:
only greeting/help/version exist. Indexing/search/MCP acceptance checks are deferred until
those features ship; this is not a v1 feature release. npm names are intended, not reserved
until the registry accepts staging/publication. Initial owner: npm user `billytheape`.

| Package | Content | Runtime target |
| --- | --- | --- |
| `repoise` | Thin Node launcher | Node >=22.14.0 |
| `repoise-linux-x64-gnu` | Rust executable | Linux x64 GNU; tested on Ubuntu 24.04 |
| `repoise-darwin-arm64` | Rust executable | macOS ARM64; tested on macOS 15 |
| `repoise-win32-x64` | Rust executable | Windows x64 MSVC; tested on Windows Server 2025 |

These are initial test baselines, not a claim of compatibility with every older OS.
Linux musl/ARM64, macOS Intel and other targets have a clear unsupported-platform error;
build Rust source for those targets. Native binaries do not need Node. Cargo registry
publication stays disabled; native source builds already work. Homebrew/containers/OS package
managers are later distribution work, not prerequisites for this release.

## One-time name claim

The bootstrap has no native binary and must not be approved as a usable release.
It establishes the `repoise` package before configuring its OIDC trusted publisher.
No npm token is needed in GitHub. Run from a reviewed checkout with Node >=22.14 and
npm >=11.15 (release CI pins npm 11.21.0):

```sh
npm whoami --registry=https://registry.npmjs.org/
npm ci --ignore-scripts
npm run release:claim
npm stage publish ./dist/claim/repoise-0.0.0-bootstrap.0.tgz --access public --tag next --registry=https://registry.npmjs.org/
```

Confirm the first command returns `billytheape`. Enable npm account 2FA before using staging.
The preparation command only packs an allowlisted launcher/LICENSE/README; it performs no
registry mutation. Staging a new package creates npm's public `0.0.0-stage` placeholder;
the bootstrap payload remains pending approval. Do not approve this bootstrap.
Review the resulting package ownership and keep the staged ID for later rejection with 2FA.
If the name is rejected or already taken, stop and settle naming before changing manifests.
The three platform package names also need ownership bootstrap; use actual generated tarballs
from the initial dry-run release bundle for that (see below), not empty placeholder packages.

## Release controls and trusted publisher settings

Create GitHub environment **npm-release** with required maintainer reviewers and main-only
workflow branch policy. The environment name in npm must match exactly. GitHub does not add
review protection automatically just because a workflow names an environment.
For each of the four npm packages, configure a trusted publisher in package settings:

| Setting | Value |
| --- | --- |
| Provider | GitHub Actions |
| Organization/user | `billy-the-ape` |
| Repository | `repoise` |
| Workflow filename | `release.yml` |
| Environment | `npm-release` |
| Allowed publishing action | Staged publication only; disable direct publication |

Use GitHub-hosted runners: npm OIDC does not currently support self-hosted runners.
Require 2FA and disable token-based direct publishing in package settings. Do not add NPM_TOKEN,
NODE_AUTH_TOKEN or bypass-2FA tokens to this repository. The npm staging job grants `id-token: write` for OIDC; a separate native draft job grants
`contents: write` for the draft GitHub Release. Build jobs remain read-only.
Use main branch protections and require CI/review as repository policy. Release preparation
also verifies that its version tag is an ancestor of origin/main, and checks out the resolved
immutable commit. PR events never invoke privileged release publishing.

## Initial release bootstrap

1. Merge the reviewed publishing PR, create `v0.1.0-alpha.0` at that merged commit, and run
   **Release preparation and staging** from **main**, with that tag and **both staging/draft options unchecked**.
2. Inspect the successful three-target builds and download the `release-bundle` artifact.
   It contains four npm tarballs, three native `.tar.gz` archives, SHA256SUMS and release.json.
   Compare release.json's SHA with the tagged commit and verify checksums before staging.
3. On your authenticated local machine, stage each of the three native package tarballs with
   `npm stage publish <tarball> --access public --tag next`. This creates their package pages.
   Configure their trusted publishers using the table above. The launcher name was already
   claimed using the separate bootstrap version.
4. Stage the actual launcher `repoise-0.1.0-alpha.0.tgz` locally too, then review and approve
   all three native staged versions with 2FA **before** approving the launcher. Native optional
   dependencies can fail silently during install if their versions are not yet publicly available.
   Verify all native packages can be installed before approving the launcher. Do not enable npm staging in the
   workflow for this same bootstrap version: it would attempt to stage duplicates.
5. Test pinned installation on all initial targets. Run the release workflow with only **draft_native** checked to create the initial
   native GitHub release draft, then review/publish it separately with its prerelease flag set. Reject the unused bootstrap stage with 2FA when ownership is established.

We walk through these steps separately during onboarding. Staging is a registry write and a
version is occupied once staged. No workflow automatically approves npm stages or publishes a
native release. Account sign-in and 2FA stay on the maintainer's own device.

## Subsequent releases

Update workspace/root npm/launcher/native dependency versions together, Cargo.lock and CHANGELOG.
Formatting, Clippy, tests and packed consumer smoke must pass. Create a matching `v<version>`
tag at a reviewed commit merged into main. Run the manual release workflow from main:

- **Both options unchecked**: prepare/test artifacts only; no npm or GitHub release writes.
- **stage_npm checked**: after environment approval, stage native tarballs and then the launcher
  through OIDC with provenance. Prereleases use npm tag `next`; stable releases use `latest`.
- **draft_native checked**: independently create an **unpublished draft** native GitHub Release.
  These separate controls allow native-only distribution and independent recovery of each channel.

Approve the native npm stages first, then the launcher. Review/publish the native draft
separately. Versions are immutable; npm staging and GitHub releases are not one transaction.
If staging or draft uploads fail, inspect partial state before retrying: the workflow deliberately
fails on duplicate versions/releases rather than skipping unknown artifacts. Continue remaining
steps manually with the exact reviewed tarballs or bump a new version after resolving state.
Do not unpublish a released version as routine rollback. Pin the previous approved version;
deprecate a faulty version and document its replacement. Index/schema migrations will need
separate versioning and rollback validation once persistence exists.

## Validation and artifacts

`npm ci --ignore-scripts`, `npm run format:check`, and `npm test` check release tooling.
CI packages/smokes the native and npm launcher on Linux, macOS and Windows. Release builds use
pinned Rust and a committed Cargo.lock, no dependency cache, and fixed runner labels. Each target
packs its native binary, then installs the native/launcher tarballs into a temporary consumer
(with spaces in its path) using offline npm and disabled install scripts. It tests help, version
and argument-error exit codes. Extracted native archives run with Node absent from PATH.
The wrapper selects exact-version optional binary packages; no models/downloads/source compilation
occur during install or invocation. Native draft attachments include native-only checksums and metadata; npm payloads stay
in the private engineering bundle until approved. The registry handles package integrity; release bundles use
SHA-256 checksums, validated before privileged staging and uploading. Checksums are integrity
checks, not independent authenticity/signing guarantees; signed native attestations remain future
work. Rust source is not packaged into npm. Native archives include LICENSE and readiness README.

Release bundles retain for 14 days; long-term published assets belong on GitHub Releases/npm.
The existing raw engineering-artifact workflow remains available but is not a release path.
Releases currently require no user runtime secrets or new environment variables. Later engine
features will document their own assets, credentials and migration requirements.

References: [npm staged publishing](https://docs.npmjs.com/staged-publishing/),
[npm stage command](https://docs.npmjs.com/cli/v11/commands/npm-stage/),
[npm trusted publishers](https://docs.npmjs.com/trusted-publishers/).
