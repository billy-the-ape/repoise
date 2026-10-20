#!/usr/bin/env node
// Affected-check selection for CI. Reads the complete merge-base diff of a
// PR (including renames, deletions and paths with spaces) and maps it to
// check sets through scripts/ci/impact-map.json. Unknown paths, missing
// merge bases, empty diffs and any selection failure resolve to the full
// check set; this tool never produces a no-op success.
import { spawnSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const defaultRoot = fileURLToPath(new URL("../..", import.meta.url));

function git(args, root) {
  const result = spawnSync("git", args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    throw new Error(
      `git ${args.join(" ")} failed: ${(result.stderr ?? "").trim()}`,
    );
  }
  return result.stdout;
}

/** Parse `git diff -z --name-status` output (NUL-separated; rename and copy
 * entries carry both old and new paths). */
export function parseNameStatusZ(output) {
  const tokens = output.split("\0").filter((token) => token.length > 0);
  const entries = [];
  let index = 0;
  while (index < tokens.length) {
    const status = tokens[index++];
    if (!/^[ACMRUD][0-9]*$/.test(status)) {
      throw new Error(`Unrecognized diff status: ${JSON.stringify(status)}`);
    }
    const oldPath = tokens[index++];
    if (oldPath === undefined)
      throw new Error("Diff entry is missing its path");
    let newPath = oldPath;
    if (status.startsWith("R") || status.startsWith("C")) {
      newPath = tokens[index++];
      if (newPath === undefined)
        throw new Error("Rename/copy entry is missing its new path");
    }
    entries.push({ status: status[0], oldPath, newPath });
  }
  return entries;
}

export function matchComponent(path, map) {
  const normalized = path.replace(/\\/g, "/").replace(/^\/+/, "");
  for (const [name, component] of Object.entries(map.components)) {
    for (const pattern of component.patterns) {
      if (pattern.endsWith("/")) {
        if (normalized.startsWith(pattern)) return name;
      } else if (normalized === pattern) {
        return name;
      }
    }
  }
  return null;
}

/** Map changed entries to components. Returns matched components, unknown
 * paths, whether the full suite is required, and the selected job keys. */
export function classifyPaths(entries, map) {
  const matched = new Map();
  const unknown = [];
  for (const entry of entries) {
    const considered =
      entry.status === "R" || entry.status === "C"
        ? [entry.oldPath, entry.newPath]
        : [entry.status === "D" ? entry.oldPath : entry.newPath];
    for (const path of considered) {
      const component = matchComponent(path, map);
      if (component === null) {
        unknown.push(path);
      } else {
        if (!matched.has(component)) matched.set(component, []);
        matched.get(component).push(path);
      }
    }
  }
  const allJobs = new Set(Object.keys(map.jobs));
  let full = false;
  const reasons = [];
  if (entries.length === 0) {
    full = true;
    reasons.push("empty diff between merge base and head");
  }
  if (unknown.length > 0) {
    full = true;
    reasons.push(`${unknown.length} unknown path(s)`);
  }
  const jobs = new Set();
  if (full) {
    for (const job of allJobs) jobs.add(job);
  } else {
    for (const component of matched.keys()) {
      for (const job of map.components[component].jobs) jobs.add(job);
    }
  }
  const reason = full
    ? reasons.join("; ")
    : `affected components: ${[...matched.keys()].join(", ")}`;
  return { full, jobs: [...jobs], components: matched, unknown, reason };
}

function parseArgs(argv) {
  const args = { base: undefined, head: undefined, repo: undefined };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--base") args.base = argv[++i];
    else if (arg === "--head") args.head = argv[++i];
    else if (arg === "--repo") args.repo = argv[++i];
    else throw new Error(`Unknown argument: ${arg}`);
  }
  return args;
}

function resolveRef(ref, root) {
  for (const candidate of [ref, `origin/${ref}`]) {
    try {
      return git(
        ["rev-parse", "--verify", `${candidate}^{commit}`],
        root,
      ).trim();
    } catch {
      // Try the next candidate.
    }
  }
  git(["fetch", "--no-tags", "--quiet", "origin", ref], root);
  return git(["rev-parse", "--verify", "FETCH_HEAD^{commit}"], root).trim();
}

function writeOutput(name, value) {
  const file = process.env.GITHUB_OUTPUT;
  if (file) appendFileSync(file, `${name}=${value}\n`);
}

function emit(result, extra) {
  for (const [name, value] of Object.entries({ ...result, ...extra })) {
    writeOutput(name, value);
    console.log(`[select] ${name}=${value}`);
  }
}

function failFull(reason) {
  emit({
    ok: "false",
    run_check: "true",
    run_docs: "true",
    components: "full",
    reason,
  });
  console.error(`[select] ERROR: ${reason}; running the full check set`);
  process.exit(1);
}

export function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  const root = args.repo ?? defaultRoot;
  const map = JSON.parse(
    readFileSync(join(root, "scripts/ci/impact-map.json"), "utf8"),
  );
  const baseRef = args.base ?? process.env.GITHUB_BASE_REF ?? "main";
  const headRef = args.head ?? "HEAD";
  try {
    const head = resolveRef(headRef, root);
    const base = resolveRef(baseRef, root);
    const mergeBase = git(["merge-base", base, head], root).trim();
    const diff = git(["diff", "-z", "--name-status", mergeBase, head], root);
    const entries = parseNameStatusZ(diff);
    const result = classifyPaths(entries, map);
    console.log(
      `[select] base=${base} head=${head} merge-base=${mergeBase} files=${entries.length}`,
    );
    for (const [component, paths] of result.components) {
      const shown = paths.slice(0, 20).join(", ");
      console.log(
        `[select]   ${component}: ${paths.length} path(s)${
          paths.length > 20 ? " (truncated display)" : ""
        }: ${shown}`,
      );
    }
    for (const path of result.unknown.slice(0, 20)) {
      console.log(`[select]   unknown: ${path}`);
    }
    if (result.unknown.length > 20)
      console.log(`[select]   unknown: (${result.unknown.length} total)`);
    emit({
      ok: "true",
      run_check: result.jobs.includes("check") ? "true" : "false",
      run_docs: result.jobs.includes("docs") ? "true" : "false",
      components: result.full
        ? "full"
        : [...result.components.keys()].join(","),
      reason: result.full ? `full: ${result.reason}` : result.reason,
    });
  } catch (error) {
    failFull(error.message ?? String(error));
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main();
