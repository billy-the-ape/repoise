import { statSync } from "node:fs";
import { join } from "node:path";

/**
 * Repoise binary discovery for the current extension host.
 *
 * Documented precedence:
 *   1. explicit user override (`repoise.cliPath`) — never falls through when
 *      set but missing, so a mistyped path is reported instead of silently
 *      running a different binary;
 *   2. a repository-local package install (`node_modules/.bin/repoise` under
 *      a workspace folder, preferred when unambiguous);
 *   3. PATH lookup in the extension host (remote-aware: the extension host
 *      runs where the workspace filesystem lives).
 *
 * Resolution is pure filesystem stat over argument-free paths: no shell, no
 * package-manager download, no execution of repository content.
 */

export type CliSource = "override" | "workspace-package" | "path";

export interface DiscoveryInput {
  /** Explicit user override; empty/undefined means "not set". */
  override?: string;
  /** Workspace folder paths (in window order) for the local-package probe. */
  workspaceRoots: string[];
}

export interface DiscoveryDeps {
  platform?: NodeJS.Platform;
  env?: NodeJS.ProcessEnv;
  /** Injectable file-existence probe (defaults to a real stat). */
  isFile?: (path: string) => boolean;
}

export interface DiscoveryResult {
  ok: boolean;
  path?: string;
  source?: CliSource;
  problem?: "not-found" | "override-not-found";
  /** Every location examined, for actionable diagnostics. */
  searched: string[];
}

export function binBasename(
  platform: NodeJS.Platform = process.platform,
): string {
  return platform === "win32" ? "repoise.cmd" : "repoise";
}

function defaultIsFile(path: string): boolean {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

function pathEntries(
  env: NodeJS.ProcessEnv,
  platform: NodeJS.Platform,
): string[] {
  const raw = (env.PATH ?? env.Path ?? "") as string;
  if (raw.length === 0) {
    return [];
  }
  return raw
    .split(platform === "win32" ? ";" : ":")
    .filter((e) => e.length > 0);
}

export function discoverCli(
  input: DiscoveryInput,
  deps: DiscoveryDeps = {},
): DiscoveryResult {
  const platform = deps.platform ?? process.platform;
  const isFile = deps.isFile ?? defaultIsFile;
  const basename = binBasename(platform);
  const searched: string[] = [];
  const override = (input.override ?? "").trim();

  if (override.length > 0) {
    searched.push(override);
    if (isFile(override)) {
      return { ok: true, path: override, source: "override", searched };
    }
    return {
      ok: false,
      problem: "override-not-found",
      searched,
    };
  }

  for (const root of input.workspaceRoots) {
    if (root.length === 0) {
      continue;
    }
    const candidate = join(root, "node_modules", ".bin", basename);
    searched.push(candidate);
    if (isFile(candidate)) {
      return {
        ok: true,
        path: candidate,
        source: "workspace-package",
        searched,
      };
    }
  }

  const env = deps.env ?? process.env;
  for (const dir of pathEntries(env, platform)) {
    const candidate = join(dir, basename);
    searched.push(candidate);
    if (isFile(candidate)) {
      return { ok: true, path: candidate, source: "path", searched };
    }
  }

  return { ok: false, problem: "not-found", searched };
}
