import { ChildProcess, spawn } from "node:child_process";

/**
 * CLI process runner. Always spawns with an argument array (never a shell),
 * keeps protocol records on stdout and diagnostics on stderr, and supports
 * cooperative cancellation: SIGTERM on Unix; on Windows a plain
 * termination (the TerminateProcess path) for native binaries, and a
 * whole-process-tree kill (`taskkill /PID <pid> /T /F`) for cmd-shim
 * launches, whose node-launcher descendants would otherwise survive.
 */

export interface CliRunOptions {
  binary: string;
  args: string[];
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  /** Hard wall-clock bound; when it fires the run is cancelled and marked timedOut. */
  timeoutMs?: number;
  /** Platform used to choose the cancellation signal and whether a .cmd/.bat shim is routed through cmd.exe (defaults to process.platform). */
  platform?: NodeJS.Platform;
  /** Called once per complete stdout line (newline characters stripped). */
  onStdoutLine?: (line: string) => void;
  /** Called with raw stderr chunks. */
  onStderr?: (text: string) => void;
  /** Test hook; defaults to child_process.spawn. */
  spawnImpl?: typeof spawn;
}

export interface CliRunResult {
  exitCode: number | null;
  signal: string | null;
  timedOut: boolean;
  cancelledByUs: boolean;
  /** Set when the binary itself could not be launched (e.g. ENOENT). */
  spawnError: string | null;
  stderr: string;
}

export interface CliRunHandle {
  promise: Promise<CliRunResult>;
  cancel: () => void;
  pid: number | null;
}

function splitLines(
  chunk: string,
  remainder: string,
  emit: (line: string) => void,
): string {
  let buf = remainder + chunk;
  for (;;) {
    const match = buf.search(/[\r\n]+/);
    if (match === -1) {
      return buf;
    }
    const line = buf.slice(0, match);
    if (line.length > 0) {
      emit(line);
    }
    buf = buf.slice(match).replace(/^[\r\n]+/, "");
  }
}

/**
 * Windows .cmd/.bat shims cannot be spawned directly: since Node
 * 18.20.2 / 20.12.2 (CVE-2024-27980) doing so throws EINVAL. Route them
 * through `cmd.exe /d /s /c` with a single payload in which every token —
 * including the shim itself — is wrapped in double quotes, and the whole
 * payload is wrapped in one further pair of quotes; inside quotes cmd.exe
 * treats every metacharacter (`&`, `|`, `<`, `>`, `^`, `(`, `)`, `!`) as
 * literal. The outer pair is required: with `/s`, cmd strips exactly one
 * leading and one trailing quote from the `/c` string (the same wrap
 * Node's own `shell: true` path uses), restoring our per-token quoting.
 * The spawn then uses `windowsVerbatimArguments` so Node does not
 * re-quote the payload.
 *
 * Arguments that can never be made literal inside a quoted cmd.exe token —
 * `"` (breaks the quoted region), `%` (variable expansion happens even
 * inside quotes) and CR/LF (line breaks) — make the whole invocation
 * fail closed: the function returns null and the caller reports a
 * spawnError instead of building a command line. Windows path names cannot
 * contain `"` in the first place, so this only fires on genuinely unsafe
 * input.
 */
export function buildWindowsShimInvocation(
  binary: string,
  args: string[],
): { binary: string; args: string[] } | null {
  const tokens = [binary, ...args];
  if (tokens.some((token) => /["%\r\n]/.test(token))) {
    return null;
  }
  const payload = `"${tokens.map((token) => `"${token}"`).join(" ")}"`;
  return { binary: "cmd.exe", args: ["/d", "/s", "/c", payload] };
}

function needsWindowsShim(platform: NodeJS.Platform, binary: string): boolean {
  return (
    platform === "win32" &&
    (binary.toLowerCase().endsWith(".cmd") ||
      binary.toLowerCase().endsWith(".bat"))
  );
}

export function runCli(options: CliRunOptions): CliRunHandle {
  const platform = options.platform ?? process.platform;
  const state = {
    timedOut: false,
    cancelledByUs: false,
    spawnError: null as string | null,
  };
  const stderrParts: string[] = [];
  let stdoutRemainder = "";
  let child: ChildProcess | null = null;
  let timer: NodeJS.Timeout | null = null;
  let settled = false;

  const emitStdoutLine = (line: string): void => {
    options.onStdoutLine?.(line);
  };

  const finish = (exitCode: number | null, signal: string | null): void => {
    if (settled) {
      return;
    }
    settled = true;
    if (timer !== null) {
      clearTimeout(timer);
    }
    if (stdoutRemainder.length > 0) {
      const line = stdoutRemainder;
      stdoutRemainder = "";
      emitStdoutLine(line);
    }
    resolve({
      exitCode,
      signal,
      timedOut: state.timedOut,
      cancelledByUs: state.cancelledByUs,
      spawnError: state.spawnError,
      stderr: stderrParts.join(""),
    });
  };

  const handle: CliRunHandle = {
    pid: null,
    cancel: (): void => {
      if (settled || child === null) {
        return;
      }
      if (child.exitCode !== null || child.signalCode !== null) {
        return;
      }
      state.cancelledByUs = true;
      try {
        // On Unix SIGTERM lets watch/index cancel cooperatively at the next
        // checkpoint (docs/automation.md).
        if (platform === "win32" && verbatimArguments) {
          // cmd.exe /d /s /c starts the npm node launcher, which starts
          // repoise: a TerminateProcess of cmd.exe alone orphans the
          // descendants — they keep the scope lock and the inherited
          // stdout/stderr pipe ends (so 'close' may never fire). Kill the
          // whole tree; taskkill runs as a plain argument-array spawn.
          const pid = child.pid;
          if (pid === null || pid === undefined) {
            child.kill();
          } else {
            const target = child;
            const taskkillChild = (options.spawnImpl ?? spawn)("taskkill", [
              "/PID",
              String(pid),
              "/T",
              "/F",
            ]);
            // A taskkill that never starts (e.g. missing on a stripped
            // host) would emit an unhandled 'error' event; fall back to
            // terminating cmd.exe itself rather than crashing the host.
            taskkillChild.on("error", () => {
              try {
                target.kill();
              } catch {
                // The process is already gone; 'close' still settles.
              }
            });
          }
        } else if (platform === "win32") {
          // A native binary (e.g. repoise.exe) has no launcher
          // descendants: plain TerminateProcess is the documented
          // abrupt-cancel semantics.
          child.kill();
        } else {
          child.kill("SIGTERM");
        }
      } catch {
        // The process is already gone; 'close' still settles the promise.
      }
    },
    promise: null as unknown as Promise<CliRunResult>,
  };

  let resolve: (result: CliRunResult) => void = () => undefined;
  handle.promise = new Promise<CliRunResult>((r) => {
    resolve = r;
  });

  let launchBinary = options.binary;
  let launchArgs = options.args;
  let verbatimArguments = false;
  if (needsWindowsShim(platform, launchBinary)) {
    const shim = buildWindowsShimInvocation(launchBinary, launchArgs);
    if (shim === null) {
      // Fail closed: rather than hand cmd.exe a string it can interpret,
      // report the run as un-launchable.
      state.spawnError =
        "unsafe characters (double quote, percent or line break) in the Windows shim invocation; refusing to build a cmd.exe command line";
      finish(null, null);
      return handle;
    }
    launchBinary = shim.binary;
    launchArgs = shim.args;
    verbatimArguments = true;
  }
  try {
    child = (options.spawnImpl ?? spawn)(launchBinary, launchArgs, {
      cwd: options.cwd,
      env: options.env ?? process.env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsVerbatimArguments: verbatimArguments,
    });
  } catch (err) {
    // Synchronous spawn failures (e.g. EINVAL when a Windows .cmd shim is
    // spawned without a shell) never reach the 'error' event.
    state.spawnError = err instanceof Error ? err.message : String(err);
    finish(null, null);
    return handle;
  }
  handle.pid = child.pid ?? null;

  child.stdout?.setEncoding("utf8");
  child.stderr?.setEncoding("utf8");
  child.stdout?.on("data", (chunk: string) => {
    stdoutRemainder = splitLines(chunk, stdoutRemainder, emitStdoutLine);
  });
  child.stderr?.on("data", (chunk: string) => {
    stderrParts.push(chunk);
    options.onStderr?.(chunk);
  });
  child.on("error", (err: Error) => {
    state.spawnError = err.message;
    finish(null, null);
  });
  child.on("close", (code, signal) => {
    finish(code, signal);
  });

  if (options.timeoutMs !== undefined && options.timeoutMs > 0) {
    timer = setTimeout(() => {
      state.timedOut = true;
      handle.cancel();
    }, options.timeoutMs);
  }

  return handle;
}
