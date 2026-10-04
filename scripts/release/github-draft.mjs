import { verifyBundle } from "./bundle.mjs";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { root } from "./common.mjs";
const directory = join(root, "dist/bundle");
const release = verifyBundle(directory, process.env.RELEASE_SHA);
const repository = process.env.GITHUB_REPOSITORY;
if (repository !== "billy-the-ape/repoise")
  throw new Error("Unexpected repository");
const headers = {
  Authorization: `Bearer ${process.env.GITHUB_TOKEN}`,
  Accept: "application/vnd.github+json",
  "X-GitHub-Api-Version": "2022-11-28",
};
const response = await fetch(
  `https://api.github.com/repos/${repository}/releases`,
  {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      tag_name: `v${release.version}`,
      target_commitish: release.sha,
      name: `Repoise ${release.version}`,
      draft: true,
      prerelease: release.version.includes("-"),
      body: "Early CLI scaffold: greeting/help/version only. Indexing is not implemented. MIT licensed. Native archives and SHA256SUMS attached; npm packages require separate staged approvals. See docs/releases.md for platform and installation requirements.",
    }),
  },
);
if (!response.ok)
  throw new Error(
    `Draft release failed (${response.status}); inspect existing releases before retrying.`,
  );
const draft = await response.json();
for (const name of [
  ...release.files.filter((file) => file.endsWith(".tar.gz")),
  "SHA256SUMS",
  "release.json",
]) {
  const url =
    draft.upload_url.replace(/\{.*$/, "") + `?name=${encodeURIComponent(name)}`;
  let payload = readFileSync(join(directory, name));
  if (name === "SHA256SUMS") {
    payload = Buffer.from(
      payload
        .toString()
        .split("\n")
        .filter((line) => line.endsWith(".tar.gz"))
        .join("\n") + "\n",
    );
  }
  if (name === "release.json") {
    payload = Buffer.from(
      JSON.stringify(
        {
          version: release.version,
          sha: release.sha,
          files: release.files.filter((file) => file.endsWith(".tar.gz")),
        },
        null,
        2,
      ) + "\n",
    );
  }
  const upload = await fetch(url, {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/octet-stream" },
    body: payload,
  });
  if (!upload.ok)
    throw new Error(
      `Upload failed for ${name} (${upload.status}); draft remains unpublished.`,
    );
}
console.log(`Created unpublished native release draft: ${draft.html_url}`);
