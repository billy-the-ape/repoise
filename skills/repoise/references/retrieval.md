# CLI and MCP retrieval examples

Use these examples with the installed version's help and advertised tool schemas.
Run CLI commands from the actual task checkout; substitute the local executable
prefix if needed (`pnpm exec repoise` or `npx --no-install repoise`). Never copy
placeholder IDs into a real read.

## CLI

```sh
repoise version
repoise status --json
# If missing/stale and refresh is permitted:
repoise index
repoise check --fresh

repoise search --json --query "YourFunctionName" --role code --max-results 5
repoise search --json --query "YourFunctionName" --path-filter "src/**" --max-results 5
repoise read --json --source-id "SOURCE_ID_FROM_RESULT" --max-output-tokens 2500
repoise related --json --source-id "SOURCE_ID_FROM_RESULT"
```

For a conceptual query with existing embedding configuration:

```sh
repoise search --json --query "retry backoff" --mode hybrid --max-results 5
```

Read the response's retrieval mode and vector coverage. The default mode remains
lexical even when embeddings are configured; request hybrid explicitly when needed.

For another search page, repeat the same query and filters:

```sh
repoise search --json --query "YourFunctionName" --role code --max-results 5 --cursor "NEXT_CURSOR_FROM_RESPONSE"
```

Diagnose missing content without changing include/exclude policy:

```sh
repoise doctor
repoise explain --path "docs/retries.md"
```

Keep secret environment values out of logs and reports. Diagnose permissions,
policy, root, snapshot mode, and freshness before assuming a ranking failure.

## MCP

Use the tool names advertised by the connected server; the client may prefix them.
Arguments use camelCase, while returned source IDs and search cursors use the
response's `source_id` and `next_cursor` fields.

| Intent                                       | Tool                        | Example arguments                                                                                |
| -------------------------------------------- | --------------------------- | ------------------------------------------------------------------------------------------------ |
| Check selected scope and freshness           | `project_knowledge_status`  | `{}`                                                                                             |
| Find a symbol                                | `search_project_knowledge`  | `{"query":"YourFunctionName","role":"code","maxResults":5}`                                      |
| Narrow a path                                | `search_project_knowledge`  | `{"query":"YourFunctionName","pathFilter":"src/**","maxResults":5}`                              |
| Conceptual search with configured embeddings | `search_project_knowledge`  | `{"query":"retry backoff","mode":"hybrid","maxResults":5}`                                       |
| Exact read                                   | `read_project_knowledge`    | `{"sourceId":"SOURCE_ID_FROM_RESULT","maxOutputTokens":2500}`                                    |
| Follow links                                 | `related_project_knowledge` | `{"sourceId":"SOURCE_ID_FROM_RESULT","limit":5}`                                                 |
| Next search page                             | `search_project_knowledge`  | `{"query":"YourFunctionName","role":"code","maxResults":5,"cursor":"NEXT_CURSOR_FROM_RESPONSE"}` |
| Refresh only when exposed and permitted      | `refresh_project_knowledge` | `{}`                                                                                             |

A server can be registered with `repoise mcp /absolute/path/to/task-checkout`;
client configuration is separate from installing this skill. `--allow-refresh`
or `mcp.refresh: true` explicitly enables the optional refresh tool. Do not add
these settings merely because a read-only server cannot refresh.

If connected to the wrong worktree, use a correctly targeted MCP server or CLI.
Neither a shared external cache nor a supplied source ID retargets the server.
