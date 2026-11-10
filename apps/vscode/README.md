# Repoise (VS Code operations extension)

A thin VS Code operations UI over an already-installed Repoise CLI: status
and freshness, live index progress, an extension-owned watch supervisor and
the Repoise stdio MCP server definition. It never implements indexing,
search, watch logic or agent behavior — every operation shells out to the
native CLI with argument arrays.

Full documentation: [`docs/vscode.md`](https://github.com/billy-the-ape/repoise/blob/main/docs/vscode.md)
in the repository (the same guide file inside this checkout's `docs/`
directory; an absolute link is used deliberately so the packaged README
resolves correctly as well).

## Prerequisites

- The native `repoise` CLI installed separately (an alpha.2-or-newer
  capability-compatible candidate is required; the extension never downloads
  or bundles a binary). Point `repoise.cliPath` at it, or keep it on PATH.
- VS Code 1.103 or newer.

## Install from a local VSIX

```sh
# from the repository root
pnpm install --frozen-lockfile
pnpm --filter repoise-vscode run package
# produces apps/vscode/repoise-vscode-<version>.vsix
code --install-extension apps/vscode/repoise-vscode-<version>.vsix
```

The `package` script does a clean TypeScript build first, so the archive
always matches the current sources. The publisher `repoise` in
`package.json` is a local preview identity, not a registered Marketplace
publisher; these VSIX files are for local install and testing only.

## Settings

| Setting                 | Meaning                                                        |
| ----------------------- | -------------------------------------------------------------- |
| `repoise.cliPath`       | Explicit binary override (highest discovery precedence)        |
| `repoise.watch.autoStart` | Start an owned watch when the status panel opens              |
| `repoise.mcp.refresh`   | Include the opt-in `refresh_project_knowledge` MCP tool        |

Registering the MCP server with VS Code does not automatically configure
every installed agent; the status panel also offers a generic stdio
`mcpServers` snippet for Cline/Codex-style clients.

## License

MIT — see [`LICENSE`](LICENSE).
