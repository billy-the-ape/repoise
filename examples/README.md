# Examples

Copy-paste-ready integration examples. These are templates, not active workflows or
installed tooling; nothing here is executed by this repository's CI.

| File | Purpose |
| --- | --- |
| [`ci/repoise-ci.example.yml`](ci/repoise-ci.example.yml) | Minimal GitHub Actions `push`/`pull_request` example: pinned install, exact checkout SHA, credential persistence disabled, an isolated per-run cache, `index` then `check --fresh`, read-only permissions, no cache restore/artifact upload and no consumer package scripts. Copy into `.github/workflows/` after substituting a published version. |
| [`consumer/package.json`](consumer/package.json) | Optional owner-invoked package scripts for `init`/`index`/`check`/`watch` on an owned checkout. |

See the [build and CI integration guide](../docs/build-ci.md) for the consumer guide,
the cache and private-corpus isolation policy, and the validation status.
