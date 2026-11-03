import { ChildProcess, spawn } from "node:child_process";

/**
 * CLI process runner. Always spawns with an argument array (never a shell),
 * keeps protocol records on stdout and diagnostics on stderr, and supports
 * cooperative cancellation: SIGTERM on Unix, plain termination (the
 * TerminateProcess path) on Windows.
 */

export interface CliRunOptions {
  binary: string;
  args: string[];
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  /** Hard wall-clock bound; when it fires the run is cancelled and marked timedOut. */
  timeoutMs?: number;
  /** Platform used to choose the cancellation signal (defaults to process.platform). */
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
        // On Windows process.kill() maps to TerminateProcess (abrupt); on
        // Unix SIGTERM lets watch/index cancel cooperatively at the next
        // checkpoint (docs/automation.md).
        if (platform === "win32") {
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

  child = (options.spawnImpl ?? spawn)(options.binary, options.args, {
    cwd: options.cwd,
    env: options.env ?? process.env,
    stdio: ["ignore", "pipe", "pipe"],
  });
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
