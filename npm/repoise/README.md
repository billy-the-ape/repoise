# Repoise

Offline-first repository knowledge for coding agents. Version 0.1.0-alpha.1 is
a functional preview: docs/code indexing, lexical search, exact source reads,
freshness checks, watch, managed agent guidance and a stdio MCP server.
The older 0.1.0-alpha.0 release contains only the initial CLI scaffold.

The launcher runs a native Rust CLI on Linux x64 GNU, macOS ARM64 and Windows
x64 MSVC. Linux musl, macOS Intel and other targets are unsupported by the
wrapper; build Rust source instead. Node >=22.14.0 is required for the launcher.
Do not omit optional dependencies: they deliver the matching executable.
Installation does not run scripts, index repositories, or download models.

After this version is published and verified, install an exact pin:

```sh
npm install --save-dev --save-exact --ignore-scripts repoise@0.1.0-alpha.1
npx --no-install repoise --help
```

From your target repository, initialize and review configuration, then index:

```sh
npx --no-install repoise init --preset docs-code-lexical --agents-snippet
npx --no-install repoise doctor
npx --no-install repoise index
npx --no-install repoise check --fresh
npx --no-install repoise search --query "YourFunctionName" --role code
```

Keep .repoise/ and repoise.local.json ignored. Use short identifier queries
and read relevant source IDs for evidence. MCP is read-only by default and
bound to its launch checkout. Watch is a separately started process.
Embeddings and remote history enrichment are opt-in.

Preview limitations include strict lexical matching, imperfect identifier
ranking, and unproven agent efficiency gains. See
[agent setup](https://github.com/billy-the-ape/repoise/blob/main/docs/agents.md)
and [release readiness](https://github.com/billy-the-ape/repoise/blob/main/docs/publish/alpha-1-readiness.md).
Native archives need no Node/npm. MIT licensed.
