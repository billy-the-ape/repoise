# Initial alpha publishing record

Snapshot: October 4, 2026 (America/Denver); operations continued into October 5 UTC.
Update after verified registry changes. This execution record is not an instruction to replay
completed operations. `0.1.0-alpha.0` is a greeting/help/version CLI scaffold only.

## Build and native publication evidence

- Publishing infrastructure [PR #3](https://github.com/billy-the-ape/repoise/pull/3) merged.
- Annotated tag `v0.1.0-alpha.0` resolves to `e75e2c11cae2d8c477f7d0cfa4bec1c24de42a02`.
- Artifact-only [run 37243008494](https://github.com/billy-the-ape/repoise/actions/runs/37243008494)
  passed all three builds/release checks/offline consumer smoke; maintainer verified all seven
  downloaded payload checksums.
- Native draft [run 37245474604](https://github.com/billy-the-ape/repoise/actions/runs/37245474604)
  passed and received environment approval; npm staging was skipped.
- Maintainer published the [native prerelease](https://github.com/billy-the-ape/repoise/releases/tag/v0.1.0-alpha.0).
  API verification confirmed public/prerelease state and all five expected assets.

## npm snapshot

| Package                 | Ownership/trust                                        | Alpha version                             |
| ----------------------- | ------------------------------------------------------ | ----------------------------------------- |
| `repoise`               | Claimed through bootstrap; trust/access settings saved | Actual alpha wrapper not staged/published |
| `repoise-linux-x64-gnu` | Owned by `billytheape`; trust/access settings saved    | Published under next                      |
| `repoise-darwin-arm64`  | Owned by `billytheape`; trust/access settings saved    | Published under next                      |
| `repoise-win32-x64`     | npm spam detection rejection; trust not configured     | Neither staged nor published              |

Saved publishers allow staging only; direct publishing/dist-tag permissions are unchecked.
Package access requires 2FA and disallows tokens. GitHub `npm-release` requires review and
allows main only; self-review prevention is unchecked for solo operation. No npm secrets added.

| Package/version                       | Stage ID                               | Outcome                                                        |
| ------------------------------------- | -------------------------------------- | -------------------------------------------------------------- |
| `repoise@0.0.0-bootstrap.0`           | `45192162-f09a-4790-9fa7-4be3db7e0060` | Pending; never approve; reject after real launcher publication |
| `repoise-linux-x64-gnu@0.1.0-alpha.0` | `2c680b32-8dba-4b75-bd30-5e9bfef05764` | Approved with 2FA and public                                   |
| `repoise-darwin-arm64@0.1.0-alpha.0`  | `e05e053d-ab7a-458d-9e7f-48fa08f1287e` | Approved with 2FA and public                                   |

Registry SHA-1 matched original tarballs: Linux `75399f160cd063e323846843e91b06e0af15ced2`;
macOS `8ec7052cd879d03ba02fe0453bb0db6c0d40c20a`. At this snapshot latest still points at
`0.0.0-stage` for those packages; the real alphas use next. Use explicit versions.

## Windows blocker and remaining tasks

Two staging attempts returned HTTP 400, `Version uniqueness check failed unexpectedly`.
Public package lookup returned 404 and authenticated stage listing returned `[]`.
Direct local publishing completed auth but returned HTTP 403, `Package name triggered spam
detection`, around October 4 at 23:42 UTC. This confirms name rejection on the direct path,
not the exact cause of the staging failure. CLI npm 11.21.0, Node 22.15.1; tarball integrity
and Windows consumer smoke passed. npm support review is pending; no ticket ID recorded.

- [ ] Obtain support clearance, recheck Windows registry/stages and publish the original alpha.
- [ ] Save Windows trusted publisher and publishing access settings.
- [ ] Verify all three native versions; stage/review/approve the original launcher.
- [ ] Record fresh pinned registry-install results on supported platforms.
- [ ] Reject unused main bootstrap after real launcher publication.
- [ ] Exercise OIDC staging on a later new reviewed version.

Follow [initial-alpha completion](npm.md#complete-the-initial-alpha-after-windows-clearance).
Do not rerun all-package staging for this version or recreate its public native release.
Native distribution is complete; npm onboarding remains partial. Indexing/search/persistence,
embeddings/MCP/watch, benchmarks and ai-gateway consumer integration have not shipped.
