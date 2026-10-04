import { appendFileSync } from "node:fs";
import { run, verifyVersion, targets } from "./common.mjs";
const tag = process.env.RELEASE_TAG;
if (!tag || !/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)?$/.test(tag))
  throw new Error("Invalid version tag");
// Only merged commits from the protected release workflow's main checkout are accepted.
const sha = run("git", ["rev-parse", "--verify", `${tag}^{commit}`]);
run("git", ["merge-base", "--is-ancestor", sha, "origin/main"]);
run("git", ["checkout", "--detach", sha]);
const version = verifyVersion(tag);
appendFileSync(
  process.env.GITHUB_OUTPUT,
  `sha=${sha}\nversion=${version}\ntag=${version.includes("-") ? "next" : "latest"}\nmatrix=${JSON.stringify({ include: targets })}\n`,
);
