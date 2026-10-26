// Shared helpers for the synthetic fake-adapter fixtures. Each fixture is a
// standalone Node script: it reads the request (the first stdin line), emits
// protocol events on stdout and exercises exactly one scenario. The host
// keeps stdin open after the request and may send control lines (e.g.
// `cancel`) on cancellation.

export function readRequest() {
  return new Promise((resolve) => {
    let buffer = "";
    const onData = (chunk) => {
      buffer += chunk;
      const index = buffer.indexOf("\n");
      if (index === -1) return;
      const line = buffer.slice(0, index);
      process.stdin.removeListener("data", onData);
      resolve(line === "" ? null : JSON.parse(line));
    };
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", onData);
  });
}

// Invokes `onLine` for every control line the host sends after the request
// (e.g. `cancel`). Blank lines are ignored.
export function watchControlLines(onLine) {
  let buffer = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, index).trim();
      buffer = buffer.slice(index + 1);
      if (line !== "") onLine(line);
    }
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
