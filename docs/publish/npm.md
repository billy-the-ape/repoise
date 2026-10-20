# npm publication workflow

## Account and trusted publishers

Maintainer: npm `billytheape`; repository: `billy-the-ape/repoise`. Use Node >=22.14.0 locally;
CI uses Node 24 and npm 11.21.0. Examples pin the same CLI through npx. Sign-in and 2FA stay
on the maintainer's device. If needed, run `npm login --registry=https://registry.npmjs.org/`
and enable account 2FA. Verify identity:

```sh
npx --yes --package=npm@11.21.0 npm whoami --registry=https://registry.npmjs.org/
```

GitHub environment `npm-release` has required maintainer review and a selected **branch**
policy allowing `main`. Self-review prevention is unchecked for the solo-maintainer setup;
revisit this when maintainers grow. Dispatch uses main even though builds resolve a tag.
Naming an environment in YAML alone does not protect it. Use main branch protection and
required CI/review as repository policy; this guide does not assert those rules are all configured.

Once each package page exists, configure its npm trusted publisher:

| Field                | Value                         |
| -------------------- | ----------------------------- |
| Provider             | GitHub Actions                |
| Organization or user | `billy-the-ape`               |
| Repository           | `repoise`                     |
| Workflow filename    | `release.yml` (filename only) |
| Environment          | `npm-release`                 |
| Allow npm publish    | Unchecked                     |
| Allow npm dist-tag   | Unchecked                     |

Staging is always allowed. Separately select **Require two-factor authentication and
disallow tokens** under publishing access. Save both settings for `repoise` and all three
native packages. See [alpha status](alpha-0-status.md) for completed setup.

OIDC uses hosted runners and `id-token: write`; the staging job has read-only repository
content access. Do not add `NPM_TOKEN`, `NODE_AUTH_TOKEN` or bypass-2FA secrets. Environment
approval authorizes staging, not npm publication. The alpha.1 OIDC attempt failed with ENEEDAUTH on the first package despite a
matching Linux trust configuration. Maintainer-local staging completed the
release; OIDC remains unresolved. See [alpha.1 execution](alpha-1-status.md).

## One-time name claims

Do not repeat claims already recorded as complete. The main-name bootstrap is prepared
from a reviewed checkout:

```sh
pnpm install --frozen-lockfile --ignore-scripts
pnpm run release:claim
npx --yes --package=npm@11.21.0 npm stage publish ./dist/claim/repoise-0.0.0-bootstrap.0.tgz --access public --tag next --registry=https://registry.npmjs.org/
```

The helper performs no registry write. Staging a new name publishes npm's `0.0.0-stage`
placeholder; the actual staged payload is private until approved. **Never approve
`0.0.0-bootstrap.0`: it has no native binary.** Keep the stage ID and reject it only after
the real launcher is published/ownership verified. Configure the main trusted publisher.
Claim native names by staging actual verified release tarballs, then configure their trust;
do not use empty native packages or rename manifests to evade a registry rejection.

## Prepare and stage a subsequent release

1. Update workspace Cargo.toml, Cargo.lock workspace entries, root package.json,
   `npm/repoise/package.json` and its exact optional dependency versions together. The
   launcher stays outside the pnpm workspace, so `pnpm-lock.yaml` tracks only root
   development dependencies and needs no bump-time update (a bump PR's frozen CI
   install must never resolve the not-yet-published platform packages). Native
   manifests are generated from the version. Update CHANGELOG/readiness; review
   dependencies, licenses and target support. Run [release checks](README.md#workflow-and-validation), get
   PR review/CI and merge into main.
2. From an up-to-date local checkout, tag the verified merged commit. Substitute version/SHA:

   ```sh
   git fetch origin --tags
   git tag -a v<version> <merged-commit-sha> -m "Repoise <version>"
   git push origin v<version>
   ```

3. Dispatch **Release preparation and staging** from main with the tag,
   `stage_npm` checked and `draft_native` checked (omit `draft_native` for
   npm-only work). One dispatch is the normal safe sequence: the run builds
   on all three platforms, assembles `release-bundle` and then pauses — both
   privileged jobs require `npm-release` approval in **Review deployments**
   before they start. Download `release-bundle` from this same run, compare
   release.json's SHA with the resolved tag, and verify all seven payloads:
   macOS `shasum -a 256 -c SHA256SUMS`, Linux `sha256sum -c SHA256SUMS`, or
   Windows `Get-FileHash -Algorithm SHA256` for each payload. Staged and
   uploaded payloads come from this exact artifact, which the privileged jobs
   re-validate before any write. Cancelling before approving publishes
   nothing.
4. Approve the staging job's `npm-release` review. It stages natives first,
   launcher last, with provenance. Staging is not publication. The
   `draft_native` job waits on its own separate approval and creates only an
   unpublished native release draft.

Prereleases use `next`, stable releases `latest`. The stage's tag is immutable. No workflow
auto-approves npm stages. A pushed version tag alone never publishes anything.

## Review, approve and verify

Inspect the intended version/tag/stage ID and compare tarball integrity with the reviewed bundle:

```sh
npx --yes --package=npm@11.21.0 npm stage list <package-name> --json --registry=https://registry.npmjs.org/
npx --yes --package=npm@11.21.0 npm stage view <stage-id> --registry=https://registry.npmjs.org/
```

The npm staged-package page or `npm stage download <stage-id>` allows payload inspection.
With this run's bundle extracted to `./dist/bundle`, the read-only helper also checks
that every package has exactly one pending stage at the release version and prints
the stage IDs in approval order. Run it through the pinned npm (npx sets
`npm_execpath`, so the helper uses npm 11.21.0, which has the `stage` subcommand),
with the maintainer's local session:

```sh
npx --yes --package=npm@11.21.0 -- node scripts/release/verify-registry.mjs stage ./dist/bundle <sha>
```

Approve **all three native versions first**, one at a time with local 2FA:

```sh
npx --yes --package=npm@11.21.0 npm stage approve <native-stage-id> --registry=https://registry.npmjs.org/
```

Verify each exact native version's integrity/shasum against the tarball and its expected tag:

```sh
npx --yes --package=npm@11.21.0 npm view <native-package>@<version> version dist.integrity dist.shasum --json --registry=https://registry.npmjs.org/
npx --yes --package=npm@11.21.0 npm view <native-package> dist-tags --json --registry=https://registry.npmjs.org/
```

After each approval, one read-only pass re-checks every published tarball against
the reviewed bundle (`dist.shasum`, expected dist-tag and the launcher's exact
optional pins):

```sh
node scripts/release/verify-registry.mjs published ./dist/bundle <sha>
```

Only then approve the launcher stage with the same approval command. Missing optional native
dependencies can be silently skipped by npm, leaving an installed wrapper without an executable.
In a fresh temporary consumer on each supported platform:

```sh
npm init -y
npm install --save-exact --ignore-scripts repoise@<version>
npx --no-install repoise --version
npx --no-install repoise --help
```

On a supported platform, the equivalent automated check installs the pinned
version in a temporary consumer and verifies version/help, invalid-argument
failure and optional-binary resolution:

```sh
node scripts/release/verify-registry.mjs install ./dist/bundle <sha>
```

Expect `repoise <version>` and successful help; verify invalid arguments return nonzero.
Record online registry-install results separately from offline tarball smoke. Index/search/MCP
checks are required for functional engine releases. Never use unversioned npx as a release test or
promote a scaffold alpha to `latest`. `latest` has pointed at the 0.1.0-alpha.1
functional preview since the October 19, 2026 promotion (it was the `0.0.0-stage`
bootstrap placeholder before); see the [alpha execution record](alpha-1-status.md).

## Complete the initial alpha after Windows clearance (historical)

This scaffold-alpha procedure was superseded, not replayed: after npm support cleared
the Windows name, the project proceeded with the functional 0.1.0-alpha.1 candidate
instead (see [alpha-0-status.md](alpha-0-status.md)). The unused main bootstrap stage
was rejected after the real launcher published, and the preview is now published under
both npm `latest` and `next` (see the [alpha execution record](alpha-1-status.md)).
The steps below are retained for audit only.

1. After npm support clears the name, recheck Windows registry/stage state. If absent, stage
   `./repoise-win32-x64-0.1.0-alpha.0.tgz` locally with pinned npm, `--access public --tag next`
   and the explicit registry. Configure Windows trust/access, then approve with 2FA.
2. Verify all three native versions. Stage the original `./repoise-0.1.0-alpha.0.tgz` locally,
   review/approve it, and perform fresh pinned registry-install checks.
3. Reject the unused main bootstrap with 2FA after the real launcher is public:

   ```sh
   npx --yes --package=npm@11.21.0 npm stage reject 45192162-f09a-4790-9fa7-4be3db7e0060 --registry=https://registry.npmjs.org/
   ```

4. Update status, README, CHANGELOG and master plan with verified results. Validate OIDC
   staging on a later new reviewed version before calling it proven.

Do not enable `stage_npm` for this alpha: the job stages every package and collides with
published native versions. Do not recreate the already public native release.

## Failure recovery

Registry stages, approvals and GitHub publication are separate writes, not a transaction.
Inspect versions/stages/releases after any failure. Resume missing steps with exact reviewed
tarballs; scripts fail on duplicates rather than guess equivalence.

| Symptom                                      | Action                                                                                                                                                                       |
| -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Occupied version                             | Inspect published versions and stages. Reject an unwanted pending candidate with 2FA after review; published versions are immutable. Resume missing packages manually.       |
| Version uniqueness check failed unexpectedly | Check registry/stages/logs; this message alone does not identify the cause. Avoid blind retries/version bumps.                                                               |
| Package name triggered spam detection        | Request [npm support](https://npmjs.com/support) review with account, package/version, source repo, timestamp and redacted log. Keep names/version unchanged pending review. |
| Auth/OIDC rejection                          | Check maintainer session/2FA or trust fields, hosted runner, main dispatch and environment approval. Do not add bypass tokens.                                               |
| Native draft/upload failure                  | Follow [native recovery](native.md#recovery); npm state is independent.                                                                                                      |

Staging-job authentication failure (the alpha.1 OIDC ENEEDAUTH remains
unresolved) is recovered by maintainer-local staging from this run's verified
bundle, exactly as alpha.1 did; never add bypass tokens or CI direct-publish
rights.

Support's optional package SHA-512 can be computed with `shasum -a 512 <tarball>`; notices
truncate integrity values. Normal maintainer `npm publish <tarball> --access public --tag next`
publishes immediately through local auth/2FA; it is not staging. The Windows fallback was also
blocked. Do not grant CI direct publishing as a workaround.
For a faulty public version, pin the previous approved version, deprecate with a replacement
explanation and publish a corrected version. Do not routinely unpublish or rewrite version tags.
Future index/schema migrations need separate rollback validation.

References: [npm stage](https://docs.npmjs.com/cli/v11/commands/npm-stage/),
[staged publishing](https://docs.npmjs.com/staged-publishing/),
[trusted publishers](https://docs.npmjs.com/trusted-publishers/).
