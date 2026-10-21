import { execFileSync } from "node:child_process";
import { readFileSync, existsSync, realpathSync } from "node:fs";
import { join, delimiter, dirname } from "node:path";
import { fileURLToPath } from "node:url";
export const root = fileURLToPath(new URL("../../", import.meta.url));
export const targets = [
  {
    target: "x86_64-unknown-linux-gnu",
    package: "repoise-linux-x64-gnu",
    os: "linux",
    cpu: "x64",
    libc: "glibc",
    binary: "repoise",
    runner: "ubuntu-24.04",
  },
  {
    target: "aarch64-apple-darwin",
    package: "repoise-darwin-arm64",
    os: "darwin",
    cpu: "arm64",
    binary: "repoise",
    runner: "macos-15",
  },
  {
    target: "x86_64-pc-windows-msvc",
    package: "repoise-win32-x64",
    os: "win32",
    cpu: "x64",
    binary: "repoise.exe",
    runner: "windows-2025",
  },
];
export function manifest() {
  return JSON.parse(
    readFileSync(new URL("../../npm/repoise/package.json", import.meta.url)),
  );
}
export function run(command, args, options = {}) {
  return execFileSync(command, args, {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
    ...options,
  }).trim();
}
/** True when a resolved path is the real npm CLI entry point, not a
 * pnpm/yarn/bun shim. Package managers may set `npm_execpath` to their own
 * CLI, so it is only trusted when it resolves to npm-cli.js. */
export function isNpmCliPath(resolved) {
  return resolved
    .replace(/\\/g, "/")
    .endsWith("node_modules/npm/bin/npm-cli.js");
}

/** Explicitly resolve the npm executable for registry operations (pack,
 * publish, stage). `npm_execpath` is honored only when it points at npm
 * itself; otherwise the PATH is searched for the npm shim (its Windows
 * `npm.cmd` maps to the sibling `npm-cli.js`). npm commands are never passed
 * to pnpm. */
export function resolveNpmCli(env = process.env, platform = process.platform) {
  let cli = env.npm_execpath;
  if (cli) {
    try {
      if (!isNpmCliPath(realpathSync(cli))) cli = undefined;
    } catch {
      cli = undefined;
    }
  }
  if (!cli) {
    for (const directory of (env.PATH ?? "").split(delimiter)) {
      const shim = join(directory, platform === "win32" ? "npm.cmd" : "npm");
      const candidate =
        platform === "win32"
          ? join(dirname(shim), "node_modules/npm/bin/npm-cli.js")
          : shim;
      if (existsSync(candidate)) {
        cli = realpathSync(candidate);
        break;
      }
    }
  }
  if (!cli)
    throw new Error(
      "npm CLI not found; run through npm scripts or install Node/npm",
    );
  return cli;
}
export function npm(args, options = {}) {
  return run(process.execPath, [resolveNpmCli(), ...args], options);
}
export function verifyVersion(tag) {
  const version = manifest().version;
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)?$/.test(version))
    throw new Error("Invalid release version");
  const cargo = run("cargo", [
    "metadata",
    "--no-deps",
    "--locked",
    "--format-version",
    "1",
  ]);
  const packages = JSON.parse(cargo).packages;
  if (packages.some((item) => item.version !== version))
    throw new Error("Cargo/npm versions differ");
  if (tag !== undefined && tag !== `v${version}`)
    throw new Error(`Tag must be v${version}`);
  const development = JSON.parse(
    readFileSync(new URL("../../package.json", import.meta.url)),
  );
  if (development.version !== version)
    throw new Error("Development package version differs");
  const wrapper = manifest();
  if (
    targets.some(
      (item) => wrapper.optionalDependencies[item.package] !== version,
    )
  )
    throw new Error("Native dependencies must use exact release versions");
  if (
    readFileSync(new URL("../../LICENSE", import.meta.url), "utf8") !==
    readFileSync(new URL("../../npm/repoise/LICENSE", import.meta.url), "utf8")
  )
    throw new Error("License copies differ");
  if (
    readFileSync(
      new URL("../../schemas/repoise.config.v1.schema.json", import.meta.url),
      "utf8",
    ) !==
    readFileSync(
      new URL(
        "../../npm/repoise/schemas/repoise.config.v1.schema.json",
        import.meta.url,
      ),
      "utf8",
    )
  )
    throw new Error("Config schema copies differ");
  return version;
}
