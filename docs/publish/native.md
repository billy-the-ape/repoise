# Native GitHub releases

Native distribution is independent of npm name availability. Archives contain the executable,
LICENSE and readiness README; no Node/npm runtime is needed. See the [target matrix](README.md).

## Prepare and publish

1. Complete version synchronization, checks, review and annotated tagging from the
   [npm preparation procedure](npm.md#prepare-and-stage-a-subsequent-release). npm publication
   itself is not required for native distribution.
2. Dispatch **Release preparation and staging** from main with `v<version>`, `stage_npm`
   unchecked for native-only work and `draft_native` checked.
3. After builds/assembly, select **Review deployments**, choose `npm-release`, then
   **Approve and deploy**. This authorizes draft creation/upload only. The native job uses
   `contents: write` and GitHub's automatic `GITHUB_TOKEN`; no stored PAT/npm token is required.
4. Review the completed run and draft: source tag/SHA, version, readiness description,
   prerelease flag and all five assets:

   - `repoise-<version>-aarch64-apple-darwin.tar.gz`
   - `repoise-<version>-x86_64-unknown-linux-gnu.tar.gz`
   - `repoise-<version>-x86_64-pc-windows-msvc.tar.gz`
   - `SHA256SUMS`
   - `release.json`

   Unlike the engineering bundle, public checksums/metadata list native archives only.
   npm payloads stay in the engineering bundle until separately approved. Compare metadata
   with the resolved tag, download/verify checksums and run extracted `repoise --version`
   and `--help` (Windows: `repoise.exe`) on the relevant platform. Current CLI has no indexing.

5. In the editor, keep **Set as a pre-release** checked for alpha/beta versions, do not designate
   as latest stable, and select **Publish release**. Verify the public assets; update
   CHANGELOG/readiness/execution status. A public native release does not imply npm availability.

Publication requires a separate maintainer action. No workflow auto-publishes a native draft.

## Recovery

Inspect existing releases/assets before repeating `draft_native`. The script creates a new
draft and fails on duplicate release state; it does not silently reuse/overwrite releases.
Upload failures leave a partial unpublished draft. Review it, then complete with verified
artifacts from the same run or remove that unpublished draft before rerunning. Never delete
or replace public releases as an automatic retry; never rewrite their version tags.
Separate build outputs need not be byte-identical: verify against their own run metadata and
record the run supplying public assets. Correct faulty public binaries through a new version
and document a known-good rollback. Checksums provide integrity, not independent signing.
