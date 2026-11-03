/**
 * Shared machine types for the AP-02 CLI automation contract.
 *
 * Normative definitions: schemas/event-stream.v1.schema.json and
 * schemas/capabilities.v1.schema.json; prose in docs/automation.md.
 * Payload fields are optional on purpose: consumers ignore unknown fields
 * and tolerate missing optional data.
 */

export interface ScopeIdentity {
  root?: string;
  repoId?: string;
  worktreeId?: string;
  snapshotMode?: string;
}

export interface StartedPayload {
  operation?: string;
  pid?: number;
  scope?: ScopeIdentity;
}

export interface ProgressPayload {
  operation?: string;
  phase?: string;
  unit?: string;
  done?: number;
  total?: number | null;
  elapsedMs?: number;
  etaSeconds?: number | null;
  generationId?: number;
  changedPaths?: string[];
  filesReparsed?: number;
  forceFull?: boolean;
  branchSwitch?: boolean;
  configChange?: boolean;
  vectorsTotal?: number;
  embedding?: Record<string, unknown> | null;
  durationMs?: number;
}

export interface WarningPayload {
  operation?: string;
  message?: string;
  kind?: string;
}

export interface TerminalPayload {
  scope?: ScopeIdentity;
  code?: string;
  message?: string;
  retryable?: boolean | null;
  reason?: string | null;
  result?: Record<string, unknown> | null;
}

export interface RepoiseEvent {
  schemaVersion: number;
  runId: string;
  sequence: number;
  timestamp: string;
  type: string;
  payload: Record<string, unknown>;
}

export function asStarted(payload: Record<string, unknown>): StartedPayload {
  return payload as StartedPayload;
}

export function asProgress(payload: Record<string, unknown>): ProgressPayload {
  return payload as ProgressPayload;
}

export function asWarning(payload: Record<string, unknown>): WarningPayload {
  return payload as WarningPayload;
}

export function asTerminal(payload: Record<string, unknown>): TerminalPayload {
  return payload as TerminalPayload;
}
