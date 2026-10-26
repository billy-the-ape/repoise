// Request, hello and event validation for the adapter host. The host enforces
// required fields and the additive compatibility policy: unknown event types
// pass with envelope-only validation; unknown event-stream versions do not.

import { CAPABILITIES, FINAL_STATUSES, PROTOCOL_VERSION } from "./protocol.js";

function isNonEmptyString(value) {
  return typeof value === "string" && value.length > 0;
}

function isAbsoluteOrBarePath(value) {
  return (
    isNonEmptyString(value) &&
    (value.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(value))
  );
}

function isPositiveInt(value) {
  return Number.isInteger(value) && value > 0;
}

function isOptionalNonNegative(value, kind) {
  if (value === null || value === undefined) return true;
  if (kind === "int") return Number.isInteger(value) && value >= 0;
  if (kind === "int-1") return isPositiveInt(value);
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

/** Validate a v1 adapter request. Returns a list of errors (empty = valid). */
export function validateRequest(request) {
  if (typeof request !== "object" || request === null) {
    return ["request must be a JSON object"];
  }
  const errors = [];
  if (request.protocolVersion !== PROTOCOL_VERSION) {
    errors.push(`protocolVersion must be ${PROTOCOL_VERSION}`);
  }

  const identity = request.identity;
  if (typeof identity !== "object" || identity === null) {
    errors.push("identity is required");
  } else {
    for (const field of ["experimentId", "trialId", "taskId"]) {
      if (!isNonEmptyString(identity[field]))
        errors.push(`identity.${field} must be a non-empty string`);
    }
    if (!Number.isInteger(identity.replication) || identity.replication < 0) {
      errors.push("identity.replication must be an integer >= 0");
    }
  }

  const workspace = request.workspace;
  if (typeof workspace !== "object" || workspace === null) {
    errors.push("workspace is required");
  } else {
    if (!isAbsoluteOrBarePath(workspace.root))
      errors.push("workspace.root must be an absolute path");
    if (!["read", "write"].includes(workspace.scope))
      errors.push('workspace.scope must be "read" or "write"');
    if (workspace.revision != null && !isNonEmptyString(workspace.revision))
      errors.push("workspace.revision must be a string or null");
    const allowedWritePaths = workspace.allowedWritePaths ?? [];
    if (!Array.isArray(allowedWritePaths) || allowedWritePaths.some((p) => !isAbsoluteOrBarePath(p)))
      errors.push("workspace.allowedWritePaths must be absolute paths");
  }

  const task = request.task;
  if (typeof task !== "object" || task === null) {
    errors.push("task is required");
  } else {
    const hasPrompt = isNonEmptyString(task.prompt);
    const hasPromptFile = isNonEmptyString(task.promptFile);
    if (hasPrompt === hasPromptFile) {
      errors.push("task must set exactly one of prompt or promptFile");
    }
    if (task.promptFile != null && !isAbsoluteOrBarePath(task.promptFile))
      errors.push("task.promptFile must be an absolute path");
    if (!isNonEmptyString(task.taskVersion))
      errors.push("task.taskVersion must be a non-empty string");
    if (!isNonEmptyString(task.taskHash))
      errors.push("task.taskHash must be a non-empty string");
  }

  const condition = request.condition;
  if (typeof condition !== "object" || condition === null) {
    errors.push("condition is required");
  } else {
    if (!["repoise", "baseline"].includes(condition.retrievalMode)) {
      errors.push('condition.retrievalMode must be "repoise" or "baseline"');
    }
    if (!Array.isArray(condition.requiredCapabilities)) {
      errors.push("condition.requiredCapabilities must be an array");
    } else {
      for (const cap of condition.requiredCapabilities) {
        if (!CAPABILITIES.includes(cap))
          errors.push(`unknown required capability: ${cap}`);
      }
    }
    const mcp = condition.mcp;
    if (mcp != null) {
      if (typeof mcp !== "object") errors.push("condition.mcp must be null or an object");
      else {
        if (!isNonEmptyString(mcp.serverName))
          errors.push("condition.mcp.serverName must be a non-empty string");
        if (!Array.isArray(mcp.argv) || mcp.argv.length < 1 || mcp.argv.some((a) => !isNonEmptyString(a)))
          errors.push("condition.mcp.argv must be a non-empty argument array");
        if (mcp.env !== undefined && typeof mcp.env !== "object")
          errors.push("condition.mcp.env must be a string map");
      }
    }
    const overlay = condition.guidanceOverlay;
    if (overlay != null && (typeof overlay !== "object" || typeof overlay.enabled !== "boolean")) {
      errors.push("condition.guidanceOverlay must be null or { enabled, description? }");
    }
  }

  const profile = request.profile;
  if (typeof profile !== "object" || profile === null) {
    errors.push("profile is required");
  } else {
    if (typeof profile.harness !== "object" || profile.harness === null) {
      errors.push("profile.harness is required");
    } else if (!isNonEmptyString(profile.harness.name)) {
      errors.push("profile.harness.name must be a non-empty string");
    }
    const provider = profile.provider;
    if (provider != null) {
      if (typeof provider !== "object" || !isNonEmptyString(provider.id)) {
        errors.push("profile.provider must be null or { id, credentialRef? }");
      } else if (
        provider.credentialRef != null &&
        !/^env:[A-Za-z_][A-Za-z0-9_]*$/.test(provider.credentialRef)
      ) {
        errors.push("profile.provider.credentialRef must be null or env:VAR_NAME");
      }
    }
    const model = profile.model;
    if (model != null && (typeof model !== "object" || !isNonEmptyString(model.id))) {
      errors.push("profile.model must be null or { id, revision? }");
    }
    const reasoning = profile.reasoning;
    if (
      reasoning != null &&
      (typeof reasoning !== "object" ||
        !["none", "low", "medium", "high", "xhigh"].includes(reasoning.level))
    ) {
      errors.push('profile.reasoning must be null or { level: none|low|medium|high|xhigh }');
    }
    const compaction = profile.compaction;
    if (
      compaction != null &&
      (typeof compaction !== "object" ||
        !["agentic", "basic", "off"].includes(compaction.mode))
    ) {
      errors.push('profile.compaction must be null or { mode: agentic|basic|off }');
    }
  }

  const limits = request.limits ?? {};
  if (typeof limits !== "object" || limits === null) {
    errors.push("limits must be an object");
  } else {
    if (!isOptionalNonNegative(limits.wallClockMs, "int-1"))
      errors.push("limits.wallClockMs must be null or an integer >= 1");
    if (!isOptionalNonNegative(limits.maxIterations, "int-1"))
      errors.push("limits.maxIterations must be null or an integer >= 1");
    if (!isOptionalNonNegative(limits.maxToolCalls, "int-1"))
      errors.push("limits.maxToolCalls must be null or an integer >= 1");
    if (!isOptionalNonNegative(limits.tokenBudget, "int-1"))
      errors.push("limits.tokenBudget must be null or an integer >= 1");
    if (!isOptionalNonNegative(limits.costBudgetUsd, "number"))
      errors.push("limits.costBudgetUsd must be null or a number > 0");
    if (!isOptionalNonNegative(limits.maxEventBytes, "int-1"))
      errors.push("limits.maxEventBytes must be null or an integer >= 1");
    if (!isOptionalNonNegative(limits.maxEvents, "int-1"))
      errors.push("limits.maxEvents must be null or an integer >= 1");
  }

  const outputs = request.outputs;
  if (typeof outputs !== "object" || outputs === null) {
    errors.push("outputs is required");
  } else {
    if (!isAbsoluteOrBarePath(outputs.artifactDir))
      errors.push("outputs.artifactDir must be an absolute path");
    for (const field of ["eventsFile", "resultFile"]) {
      if (outputs[field] != null && !isAbsoluteOrBarePath(outputs[field]))
        errors.push(`outputs.${field} must be an absolute path or null`);
    }
  }

  return errors;
}

/** Validate the hello handshake event. Returns a list of errors. */
export function validateHello(hello) {
  const errors = [];
  if (typeof hello !== "object" || hello === null) return ["hello is not an object"];
  if (hello.protocolVersion !== PROTOCOL_VERSION) {
    errors.push(`hello.protocolVersion must be ${PROTOCOL_VERSION}`);
  }
  if (typeof hello.adapter !== "object" || hello.adapter === null) {
    errors.push("hello.adapter is required");
  } else {
    for (const field of ["name", "version"]) {
      if (!isNonEmptyString(hello.adapter[field]))
        errors.push(`hello.adapter.${field} must be a non-empty string`);
    }
  }
  if (typeof hello.capabilities !== "object" || hello.capabilities === null) {
    errors.push("hello.capabilities is required");
  } else {
    for (const cap of CAPABILITIES) {
      if (typeof hello.capabilities[cap] !== "boolean") {
        errors.push(`hello.capabilities.${cap} must be a boolean`);
      }
    }
  }
  if (typeof hello.usage !== "object" || hello.usage === null) {
    errors.push("hello.usage is required");
  } else if (!["cumulative", "delta"].includes(hello.usage.eventSemantics)) {
    errors.push('hello.usage.eventSemantics must be "cumulative" or "delta"');
  }
  return errors;
}

/** Envelope-only validation for any event (additive compatibility). */
export function validateEventEnvelope(event) {
  const errors = [];
  if (typeof event !== "object" || event === null) errors.push("event is not an object");
  else {
    if (event.v !== PROTOCOL_VERSION) errors.push(`event.v must be ${PROTOCOL_VERSION}`);
    if (!isNonEmptyString(event.t)) errors.push("event.t must be a non-empty string");
    if (typeof event.ts !== "number" || !Number.isFinite(event.ts))
      errors.push("event.ts must be a number");
  }
  return errors;
}

/** Validate a final event payload. Returns a list of errors. */
export function validateFinalEvent(event) {
  const errors = [];
  if (typeof event.status !== "string" || !FINAL_STATUSES.includes(event.status)) {
    errors.push(`final.status must be one of: ${FINAL_STATUSES.join(", ")}`);
  }
  if (event.reason != null && !isNonEmptyString(event.reason)) {
    errors.push("final.reason must be a string or null");
  }
  return errors;
}
