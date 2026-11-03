import { join } from "node:path";
import { EventStream } from "../core/events";
import { RepoiseEvent } from "../core/types";
import { runCli, CliRunHandle } from "../core/run";

/** Compiled-test-tree location of the deterministic fake CLI. */
export const FAKE = join(__dirname, "fixtures", "fake-repoise.mjs");

export interface FakeRun {
  handle: CliRunHandle;
  stream: EventStream;
  events: RepoiseEvent[];
  firstStarted: Promise<RepoiseEvent | null>;
}

export function runFake(
  args: string[],
  env: Record<string, string> = {},
): FakeRun {
  const stream = new EventStream();
  const events: RepoiseEvent[] = [];
  let startedResolve: (event: RepoiseEvent | null) => void = () => undefined;
  const firstStarted = new Promise<RepoiseEvent | null>((resolve) => {
    startedResolve = resolve;
  });
  const handle = runCli({
    binary: process.execPath,
    args: [FAKE, ...args],
    env: { ...process.env, ...env },
    onStdoutLine: (line) => {
      const outcome = stream.feed(line);
      if (outcome.kind === "event") {
        events.push(outcome.event);
        if (outcome.event.type === "started") {
          startedResolve(outcome.event);
        }
      }
    },
  });
  return { handle, stream, events, firstStarted };
}

export function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
