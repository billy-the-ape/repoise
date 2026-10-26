#!/usr/bin/env node
// Declares that it cannot provide tool telemetry. A trial that requires
// tool_telemetry must fail preflight rather than be approximated.
import { hello, readRequest } from "./helpers.mjs";

await readRequest();
hello({
  capabilities: {
    fresh_sessions: true,
    isolated_state: true,
    mcp_config: true,
    read_only_restriction: true,
    tool_telemetry: false,
    usage_semantics: true,
    model_selection: true,
    cancellation: true,
  },
});
process.exit(0);
