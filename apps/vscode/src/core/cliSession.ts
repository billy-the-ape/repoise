import { CapabilitiesDoc } from "./capabilities";
import { CliSource } from "./discovery";

/**
 * Resolved CLI session and its failure shapes.
 *
 * Kept in core (vscode-free) so both the extension-host glue
 * (`ui/cliManager`) and the watch supervisor (`ui/watchManager`) can share
 * the contract without pulling the vscode module into core/test code.
 */

export interface CliSession {
  binary: string;
  source: CliSource;
  version: string;
  compatible: boolean;
  problems: string[];
  searched: string[];
  capabilities: CapabilitiesDoc;
}

export type CliFailure =
  | { problem: "not-found"; message: string }
  | { problem: "override-not-found"; message: string }
  | { problem: "capabilities-failed"; message: string }
  | { problem: "incompatible"; message: string };

export function isCliSession(
  value: CliSession | CliFailure,
): value is CliSession {
  return "binary" in value;
}
