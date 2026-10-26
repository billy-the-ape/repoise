// Deliberate cross-platform launching rules for adapter processes.
// - Executables are always launched directly (no shell template), so quoting
//   paths with spaces never invokes a shell.
// - POSIX: the adapter runs in its own process group (detached spawn) so the
//   coordinator can kill the whole owned tree and clean only what it owns.
// - Windows: signals cannot be delivered to console-less Node processes
//   (`taskkill` without `/F` only posts WM_CLOSE to windows, of which there
//   are none), so cooperative cancellation travels on the host's stdin
//   `cancel` control line and the escalation is a forced `taskkill /F /T`
//   of the owned tree.

import { execFile, spawn } from "node:child_process";

export function spawnAdapterProcess({ executable, args = [], env, cwd }) {
  const options = {
    env,
    cwd,
    stdio: ["pipe", "pipe", "pipe"],
    shell: false,
  };
  if (process.platform !== "win32") options.detached = true;
  return spawn(executable, args, options);
}

/** Send a signal to the owned process tree. Resolves even if the process is
 * already gone. On Windows SIGINT is a no-op by design: graceful
 * cancellation must use the host's stdin `cancel` control line, and only
 * forced signals (SIGKILL → `taskkill /F /T`) are implemented. */
export function killTree(pid, signal) {
  return new Promise((resolve) => {
    if (pid === undefined || pid === null) return resolve();
    if (process.platform === "win32") {
      if (signal === "SIGINT") return resolve(); // No graceful signal exists.
      execFile(
        "taskkill",
        ["/PID", String(pid), "/T", "/F"],
        { shell: false },
        () => resolve(),
      );
    } else {
      try {
        process.kill(-pid, signal); // Whole owned process group.
        resolve();
      } catch {
        try {
          process.kill(pid, signal); // Fallback: the group may already be gone.
        } catch {
          // Already dead.
        }
        resolve();
      }
    }
  });
}
