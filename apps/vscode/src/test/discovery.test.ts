import assert from "node:assert/strict";
import { join } from "node:path";
import { test } from "node:test";
import { binBasenames, discoverCli } from "../core/discovery";

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

test("binBasenames follows the platform", () => {
  assert.deepEqual(binBasenames("win32"), ["repoise.cmd", "repoise.exe"]);
  assert.deepEqual(binBasenames("linux"), ["repoise"]);
  assert.deepEqual(binBasenames("darwin"), ["repoise"]);
});

test("PATH fallback finds a native repoise.exe on win32", () => {
  const dir = "C:\\native";
  const candidate = join(dir, "repoise.exe");
  const result = discoverCli(
    { workspaceRoots: [] },
    {
      platform: "win32",
      env: { PATH: `C:\\tools;${dir}` },
      isFile: (p: string) => p === candidate,
    },
  );
  assert.equal(result.ok, true);
  assert.equal(result.source, "path");
  assert.equal(result.path, candidate);
});

test("on win32 the npm shim wins in the same directory as repoise.exe", () => {
  const dir = "C:\\both";
  const shim = join(dir, "repoise.cmd");
  const exe = join(dir, "repoise.exe");
  const result = discoverCli(
    { workspaceRoots: [] },
    {
      platform: "win32",
      env: { PATH: dir },
      isFile: (p: string) => p === shim || p === exe,
    },
  );
  assert.equal(result.ok, true);
  assert.equal(result.path, shim);
});

test("on win32 PATH directory order is preserved across bin names", () => {
  const exeDir = "C:\\native";
  const shimDir = "C:\\npm";
  const shim = join(shimDir, "repoise.cmd");
  const exe = join(exeDir, "repoise.exe");
  const result = discoverCli(
    { workspaceRoots: [] },
    {
      platform: "win32",
      env: { PATH: `${exeDir};${shimDir}` },
      isFile: (p: string) => p === shim || p === exe,
    },
  );
  // The exe's directory comes first in PATH, so it wins over the shim in a
  // later directory.
  assert.equal(result.ok, true);
  assert.equal(result.path, exe);
});

test("a repoise.exe in a workspace .bin resolves when no shim exists", () => {
  const candidate = join("C:\\ws", "node_modules", ".bin", "repoise.exe");
  const result = discoverCli(
    { workspaceRoots: ["C:\\ws"] },
    {
      platform: "win32",
      env: { PATH: "" },
      isFile: (p: string) => p === candidate,
    },
  );
  assert.equal(result.ok, true);
  assert.equal(result.source, "workspace-package");
  assert.equal(result.path, candidate);
});
