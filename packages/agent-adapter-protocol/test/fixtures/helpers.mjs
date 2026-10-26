// Shared helpers for the synthetic fake-adapter fixtures. Each fixture is a
// standalone Node script: it drains the request on stdin, emits protocol
// events on stdout and exercises exactly one scenario.

export function readRequest() {
  return new Promise((resolve) => {
    let data = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => (data += chunk));
    process.stdin.on("end", () =>
      resolve(data === "" ? null : JSON.parse(data)),
    );
  });
}

export function emit(obj) {
  process.stdout.write(`${JSON.stringify(obj)}\n`);
}

export function hello(overrides = {}) {
  emit({
    v: 1,
    t: "hello",
    ts: Date.now(),
    adapter: { name: "fake", version: "1.0.0" },
    protocolVersion: 1,
    capabilities: {
      fresh_sessions: true,
      isolated_state: true,
      mcp_config: true,
      read_only_restriction: true,
      tool_telemetry: true,
      usage_semantics: true,
      model_selection: true,
      cancellation: true,
    },
    usage: {
      eventSemantics: "delta",
      inputIncludesCacheReads: null,
      includesSubagents: null,
      includesCompaction: null,
      costSemantics: "provider",
    },
    harness: { surface: "test", revision: "1.0.0", revisionSource: "pinned" },
    ...overrides,
  });
}
