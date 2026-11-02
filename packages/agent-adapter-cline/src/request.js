// Adapter-side request validation. The protocol host performs full
// validation (schemas/adapter-request.v1.schema.json is authoritative); this
// is a defensive re-check so a mis-routed adapter still fails with a
// protocol_error final instead of a crash.

function isNonEmptyString(value) {
  return typeof value === "string" && value.length > 0;
}

function isAbsolute(value) {
  return (
    isNonEmptyString(value) &&
    (value.startsWith("/") || /^[a-zA-Z]:[\\/]/.test(value))
  );
}

export function validateRequest(request) {
  const errors = [];
  if (typeof request !== "object" || request === null) {
    return ["request must be a JSON object"];
  }
  if (request.protocolVersion !== 1) errors.push("protocolVersion must be 1");
  const identity = request.identity;
  if (typeof identity !== "object" || identity === null)
    errors.push("identity is required");
  else {
    for (const field of ["experimentId", "trialId", "taskId"]) {
      if (!isNonEmptyString(identity[field]))
        errors.push(`identity.${field} is required`);
    }
    if (!Number.isInteger(identity.replication) || identity.replication < 0) {
      errors.push("identity.replication must be an integer >= 0");
    }
  }
  const workspace = request.workspace;
  if (typeof workspace !== "object" || workspace === null)
    errors.push("workspace is required");
  else {
    if (!isAbsolute(workspace.root))
      errors.push("workspace.root must be an absolute path");
    if (!["read", "write"].includes(workspace.scope))
      errors.push('workspace.scope must be "read" or "write"');
  }
  const task = request.task;
  if (typeof task !== "object" || task === null)
    errors.push("task is required");
  else {
    const hasPrompt = isNonEmptyString(task.prompt);
    const hasPromptFile = isNonEmptyString(task.promptFile);
    if (hasPrompt === hasPromptFile) {
      errors.push("task must set exactly one of prompt or promptFile");
    } else if (hasPrompt && task.prompt.startsWith("-")) {
      // The prompt travels as a trailing positional argv element; Cline
      // 3.0.74 does not honour a `--` end-of-options guard and rejects
      // dash-prefixed prompts ("Unknown command or unquoted prompt").
      errors.push(
        "task.prompt must not start with '-' (it would be parsed as a Cline CLI option)",
      );
    }
  }
  const condition = request.condition;
  if (typeof condition !== "object" || condition === null)
    errors.push("condition is required");
  else if (!["repoise", "baseline"].includes(condition.retrievalMode)) {
    errors.push('condition.retrievalMode must be "repoise" or "baseline"');
  }
  const profile = request.profile;
  if (typeof profile !== "object" || profile === null)
    errors.push("profile is required");
  else if (
    typeof profile.harness !== "object" ||
    profile.harness === null ||
    !isNonEmptyString(profile.harness.name)
  ) {
    errors.push("profile.harness.name is required");
  } else {
    // The optional expected harness version pin must be a dotted version
    // (matching the version-probe parsing), so a malformed pin fails
    // validation here instead of later string parsing in the adapter.
    const revision = profile.harness.revision;
    if (
      revision != null &&
      !/^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][A-Za-z0-9][A-Za-z0-9_.]*)?$/.test(
        revision,
      )
    ) {
      errors.push(
        "profile.harness.revision must be null or a dotted version like 3.0.74",
      );
    }
  }
  // The adapter is a standalone CLI: do not rely on the host's validation.
  // Only env: references can be handled safely; anything else (including a
  // raw key string) is rejected instead of being recorded verbatim.
  const provider = profile?.provider;
  if (
    typeof provider === "object" &&
    provider !== null &&
    provider.credentialRef != null &&
    (typeof provider.credentialRef !== "string" ||
      !/^env:[A-Za-z_][A-Za-z0-9_]*$/.test(provider.credentialRef))
  ) {
    errors.push("profile.provider.credentialRef must be null or env:VAR_NAME");
  }
  const outputs = request.outputs;
  if (typeof outputs !== "object" || outputs === null)
    errors.push("outputs is required");
  else if (!isAbsolute(outputs.artifactDir)) {
    errors.push("outputs.artifactDir must be an absolute path");
  }
  const limits = request.limits;
  if (limits !== undefined && typeof limits !== "object")
    errors.push("limits must be an object");
  return errors;
}
