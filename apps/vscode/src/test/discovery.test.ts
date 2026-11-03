import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { binBasename, discoverCli } from "../core/discovery";

test("override wins and is reported; a missing override never falls through", () => {
  const files = new Set(["/opt/bin/repoise", "/usr/bin/repoise"]);
  const isFile = (p: string) => files.has(p);
  const deps = {
    platform: "linux" as const,
    env: { PATH: "/usr/bin" },
    isFile,
  };

  const found = discoverCli(
    { override: "/opt/bin/repoise", workspaceRoots: ["/ws"] },
    deps,
  );
  assert.equal(found.ok, true);
  assert.equal(found.source, "override");
  assert.equal(found.path, "/opt/bin/repoise");

  const missing = discoverCli(
    { override: "/nope/repoise", workspaceRoots: ["/ws"] },
    deps,
  );
  assert.equal(missing.ok, false);
  assert.equal(missing.problem, "override-not-found");
  assert.equal(missing.path, undefined);
});

test("workspace-local package is preferred over PATH and keeps spaces", () => {
  const candidate = join("/ws one", "node_modules", ".bin", "repoise");
  const files = new Set([candidate, join("/usr/bin", "repoise")]);
  const result = discoverCli(
    { workspaceRoots: ["/ws one", "/other"] },
    {
      platform: "linux",
      env: { PATH: "/usr/bin" },
      isFile: (p: string) => files.has(p),
    },
  );
  assert.equal(result.ok, true);
  assert.equal(result.source, "workspace-package");
  assert.equal(result.path, candidate);
});

test("PATH fallback works and uses the Windows bin name on win32", () => {
  const dir = "C:\\Program Files\\Repoise";
  const candidate = join(dir, "repoise.cmd");
  const files = new Set([candidate]);
  const result = discoverCli(
    { workspaceRoots: [] },
    {
      platform: "win32",
      env: { PATH: `C:\\tools;${dir}` },
      isFile: (p: string) => files.has(p),
    },
  );
  assert.equal(result.ok, true);
  assert.equal(result.source, "path");
  assert.equal(result.path, candidate);
});

test("not-found lists every searched location", () => {
  const result = discoverCli(
    { workspaceRoots: ["/ws"] },
    { platform: "linux", env: { PATH: "/usr/bin" }, isFile: () => false },
  );
  assert.equal(result.ok, false);
  assert.equal(result.problem, "not-found");
  assert.ok(
    result.searched.includes(join("/ws", "node_modules", ".bin", "repoise")),
  );
  assert.ok(result.searched.includes(join("/usr/bin", "repoise")));
});

test("binBasename follows the platform", () => {
  assert.equal(binBasename("win32"), "repoise.cmd");
  assert.equal(binBasename("linux"), "repoise");
  assert.equal(binBasename("darwin"), "repoise");
});
