const { createRequire } = require("node:module");
const { join } = require("node:path");

function packageFor(platform, arch, report) {
  if (
    platform === "linux" &&
    arch === "x64" &&
    report?.header?.glibcVersionRuntime
  )
    return "repoise-linux-x64-gnu";
  if (platform === "darwin" && arch === "arm64") return "repoise-darwin-arm64";
  if (platform === "win32" && arch === "x64") return "repoise-win32-x64";
  throw new Error(
    `Unsupported platform ${platform}/${arch}. Initial targets: Linux x64 GNU, macOS ARM64, Windows x64 MSVC. Build the Rust source for other targets.`,
  );
}

function binaryPath(platform, arch, report, resolve = require.resolve) {
  const name = packageFor(platform, arch, report);
  let manifest;
  try {
    manifest = resolve(`${name}/package.json`);
  } catch {
    throw new Error(
      `Missing ${name}. Reinstall repoise with optional dependencies enabled; no binary is downloaded automatically.`,
    );
  }
  const installed = createRequire(manifest)(manifest);
  const expected = require("../package.json").version;
  if (installed.version !== expected)
    throw new Error(
      `Native package version ${installed.version} does not match launcher ${expected}. Reinstall the pinned release.`,
    );
  return join(
    manifest,
    "..",
    "bin",
    platform === "win32" ? "repoise.exe" : "repoise",
  );
}
module.exports = { packageFor, binaryPath };
