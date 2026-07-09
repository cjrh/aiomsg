#!/usr/bin/env node
/**
 * Bump the repo release version, create an annotated semver tag, and push both.
 *
 * This script intentionally keeps the public interface small: pass `major`,
 * `minor`, `patch`, or an exact `x.y.z` version. Add future package manifests
 * to VERSIONED_FILES so one repo tag continues to describe one coherent release.
 */
import { execFileSync, spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const VERSIONED_FILES = ["browser-lib/package.json"];
const REMOTE = process.env.AIOMSG_RELEASE_REMOTE ?? "origin";
const VERSION_RE = /^(\d+)\.(\d+)\.(\d+)$/;

function fail(message) {
  console.error(`release: ${message}`);
  process.exit(1);
}

function run(command, args, { capture = false, allowFailure = false, cwd } = {}) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: "utf8",
    stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
  });
  if (result.status !== 0 && !allowFailure) {
    if (capture && result.stderr) process.stderr.write(result.stderr);
    fail(`command failed: ${command} ${args.join(" ")}`);
  }
  return capture ? result.stdout.trim() : "";
}

function parseVersion(value) {
  const match = VERSION_RE.exec(value);
  if (!match) fail(`expected a stable SemVer version (x.y.z), got ${value}`);
  return match.slice(1).map(Number);
}

function formatVersion(parts) {
  return parts.join(".");
}

function compareVersions(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return 0;
}

function nextVersion(current, bump) {
  const parts = parseVersion(current);
  if (bump === "major") return formatVersion([parts[0] + 1, 0, 0]);
  if (bump === "minor") return formatVersion([parts[0], parts[1] + 1, 0]);
  if (bump === "patch") return formatVersion([parts[0], parts[1], parts[2] + 1]);

  const exact = bump.replace(/^v/, "");
  const exactParts = parseVersion(exact);
  if (compareVersions(exactParts, parts) < 0) {
    fail(`exact version ${exact} is older than current version ${current}`);
  }
  return exact;
}

function readPackageVersion(path) {
  return JSON.parse(readFileSync(path, "utf8")).version;
}

function writePackageVersion(path, version) {
  const pkg = JSON.parse(readFileSync(path, "utf8"));
  pkg.version = version;
  writeFileSync(path, `${JSON.stringify(pkg, null, 2)}\n`);
}

function commandSucceeds(command, args) {
  const result = spawnSync(command, args, { stdio: "ignore" });
  return result.status === 0;
}

const bump = process.argv[2];
if (!bump || !(bump === "major" || bump === "minor" || bump === "patch" || /^v?\d+\.\d+\.\d+$/.test(bump))) {
  fail("usage: just release <major|minor|patch|x.y.z>");
}

const root = execFileSync("git", ["rev-parse", "--show-toplevel"], { encoding: "utf8" }).trim();
process.chdir(root);

const branch = run("git", ["branch", "--show-current"], { capture: true });
if (!branch) fail("cannot release from a detached HEAD");

const dirtyTracked = run("git", ["status", "--porcelain=v1", "--untracked-files=no"], { capture: true });
if (dirtyTracked) {
  fail(`tracked working tree changes are present; commit or stash them first\n${dirtyTracked}`);
}

const versions = VERSIONED_FILES.map((path) => [path, readPackageVersion(path)]);
const current = versions[0][1];
for (const [path, version] of versions) {
  if (version !== current) {
    fail(`version mismatch: ${path} has ${version}, expected ${current}`);
  }
}

const releaseVersion = nextVersion(current, bump);
const tag = `v${releaseVersion}`;

console.log(`Preparing ${tag} from ${branch}`);
run("git", ["fetch", "--tags", REMOTE]);

if (commandSucceeds("git", ["rev-parse", "--verify", "--quiet", `refs/tags/${tag}`])) {
  fail(`local tag ${tag} already exists`);
}
const remoteTag = run("git", ["ls-remote", "--tags", REMOTE, `refs/tags/${tag}`], { capture: true });
if (remoteTag) fail(`remote tag ${tag} already exists on ${REMOTE}`);

console.log("Running browser package tests...");
run("just", ["test-browser"]);

if (releaseVersion !== current) {
  for (const path of VERSIONED_FILES) writePackageVersion(path, releaseVersion);
  console.log(`Updated ${VERSIONED_FILES.join(", ")} from ${current} to ${releaseVersion}`);
} else {
  console.log(`Version is already ${releaseVersion}; tagging current HEAD without a version commit.`);
}

console.log("Checking npm package contents...");
run("npm", ["pack", "--dry-run"], { cwd: "browser-lib" });

if (releaseVersion !== current) {
  run("git", ["add", ...VERSIONED_FILES]);
  run("git", ["commit", "-m", `Release ${tag}`]);
}

run("git", ["tag", "-a", tag, "-m", `aiomsg ${releaseVersion}`]);
run("git", ["push", "--atomic", REMOTE, `HEAD:refs/heads/${branch}`, `refs/tags/${tag}`]);

console.log(`Release ${tag} pushed. GitHub Actions will publish release packages.`);
