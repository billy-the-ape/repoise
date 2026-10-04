# Repoise

Offline-first repository knowledge for coding agents. **Early scaffold:** greeting,
help and version only; indexing/search/MCP are not implemented yet.

The npm launcher runs the same Rust CLI as native archives. Initial binary targets:
Linux x64 GNU, macOS ARM64 and Windows x64 MSVC. Linux musl, macOS Intel and other
architectures are unsupported by this initial wrapper; build the Rust source instead.
Node >=22.14.0 is required only for this launcher. There are no install scripts,
network downloads on invocation, model downloads or repository edits.
Do not omit optional dependencies: they deliver the matching native executable.

Pin an approved version in automation. See the repository's
[release guide](https://github.com/billy-the-ape/repoise/blob/main/docs/releases.md)
for readiness, installation, native builds and release limitations. MIT licensed.

After the prerelease is approved on npm, evaluate a pinned version:

```sh
npx --package=repoise@0.1.0-alpha.0 repoise --help
```

For project automation, add that exact version as a dev dependency using your package
manager (npm, pnpm, yarn or bun) and invoke its local `repoise` command.
