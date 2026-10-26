// The Cline adapter's hello payload. Capability honesty matters: the CLI
// surface exposes no per-tool events and no read-only enforcement, so both
// are declared false rather than approximated (verified against the Cline
// CLI reference, 2026-10-19; live behavior unmeasured).

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pkg = JSON.parse(readFileSync(join(here, "..", "package.json"), "utf8"));

export function helloEvent() {
  return {
    v: 1,
    t: "hello",
    ts: Date.now(),
    adapter: { name: "cline", version: pkg.version },
    protocolVersion: 1,
    capabilities: {
      fresh_sessions: true,
      isolated_state: true,
      mcp_config: true,
      read_only_restriction: false,
      tool_telemetry: false,
      usage_semantics: true,
      model_selection: true,
      cancellation: true,
    },
    harness: { surface: "cli", revision: null, revisionSource: "runtime" },
    usage: {
      eventSemantics: "cumulative",
      inputIncludesCacheReads: null,
      includesSubagents: null,
      includesCompaction: null,
      costSemantics: "provider",
    },
  };
}

export const ADAPTER_VERSION = pkg.version;
