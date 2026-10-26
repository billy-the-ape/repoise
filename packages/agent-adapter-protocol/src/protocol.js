// Protocol v1 constants shared by the coordinator host and all adapters.
// Authoritative schemas: schemas/adapter-request.v1.schema.json and
// schemas/adapter-events.v1.schema.json.

export const PROTOCOL_VERSION = 1;

// Capabilities an adapter advertises in its hello event. Missing required
// capabilities fail preflight; they are never silently approximated.
export const CAPABILITIES = [
  "fresh_sessions",
  "isolated_state",
  "mcp_config",
  "read_only_restriction",
  "tool_telemetry",
  "usage_semantics",
  "model_selection",
  "cancellation",
];

// Final terminal statuses. Independent evaluation owns correctness; these
// describe process outcomes, not task success.
export const FINAL_STATUSES = [
  "completed",
  "agent_error",
  "timeout",
  "cancelled",
  "budget_stop",
  "adapter_error",
  "protocol_error",
];

// Known v1 event types. Hosts must still accept unknown types (additive
// compatibility); this list is used for documentation and tests.
export const EVENT_TYPES = [
  "hello",
  "started",
  "note",
  "model_request_start",
  "model_request_end",
  "tool_start",
  "tool_end",
  "usage",
  "artifact",
  "warning",
  "final",
];

export const USAGE_FIELDS = [
  "inputTokens",
  "outputTokens",
  "reasoningTokens",
  "cacheReadTokens",
  "cacheWriteTokens",
  "costUsd",
];

// Defaults for bounded streaming (request.limits may override both).
export const DEFAULT_MAX_EVENT_BYTES = 65_536;
export const DEFAULT_MAX_EVENTS = 20_000;
export const DEFAULT_HELLO_TIMEOUT_MS = 10_000;
export const DEFAULT_CANCEL_GRACE_MS = 5_000;
