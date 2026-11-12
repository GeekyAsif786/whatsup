// Assembles the npm packages from the release archives.
//
//   node scripts/npm-assemble.mjs --version 0.1.0 --archives dist --out npm-dist
//
// `--archives` holds the five whatsup-v<version>-<target>.{tar.gz,zip} files
// and SHA256SUMS from a GitHub Release. Every archive is checked against
// SHA256SUMS first. The result is one ready-to-publish directory per package:
//
//   npm-dist/darwin-arm64/{package.json, README.md, LICENSE, bin/whatsup}
//   ... (darwin-x64, linux-arm64, linux-x64, win32-x64: bin/whatsup.exe)
//   npm-dist/whatsup-cli/{package.json, README.md, LICENSE, bin/whatsup.js}
//
// The package.json files are copied from packages/ unchanged; their versions
// must already equal --version (bump them with Cargo.toml, see RELEASE.md).
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const PACKAGES = path.join(ROOT, "packages");

/** npm package directory → Rust target of the binary it carries. */
export const TARGETS = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "win32-x64": "x86_64-pc-windows-msvc",
};

export function archiveName(version, target) {
  const ext = target.includes("windows") ? "zip" : "tar.gz";
  return `whatsup-v${version}-${target}.${ext}`;
}

/** Parses sha256sum output ("<hex>  <name>" or "<hex> *<name>") into a Map. */
export function parseSums(text) {
  const sums = new Map();
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const m = line.match(/^([0-9a-f]{64}) [ *](.+)$/);
    if (!m) throw new Error(`SHA256SUMS: bad line ${JSON.stringify(line)}`);
    sums.set(m[2], m[1]);
  }
  return sums;
}

/** Throws unless every package.json in packages/ is at `version` and the
 * main package depends on exactly the platform packages, at `version`. */
export function checkVersions(version, packagesDir = PACKAGES) {
  const read = (dir) => JSON.parse(fs.readFileSync(path.join(packagesDir, dir, "package.json"), "utf8"));
  const problems = [];
  for (const dir of [...Object.keys(TARGETS), "whatsup-cli"]) {
    const pkg = read(dir);
    if (pkg.version !== version) problems.push(`packages/${dir}: version ${pkg.version}, expected ${version}`);
  }
  const deps = read("whatsup-cli").optionalDependencies ?? {};
  const wanted = Object.keys(TARGETS).map((d) => `@whatsup-cli/${d}`);
  if (Object.keys(deps).sort().join() !== wanted.sort().join()) {
    problems.push(`packages/whatsup-cli: optionalDependencies should be exactly ${wanted.join(", ")}`);
  }
  for (const [name, v] of Object.entries(deps)) {
    if (v !== version) problems.push(`packages/whatsup-cli: ${name} at ${v}, expected ${version}`);
  }
  if (problems.length) throw new Error(problems.join("\n"));
}

function sha256(file) {
  return createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

/** Extracts `archive` into a fresh temp dir and returns that dir. */
function extract(archive) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "whatsup-assemble-"));
  // bsdtar (macOS, Windows) reads zip too; GNU tar (Linux) doesn't.
  if (archive.endsWith(".zip") && process.platform === "linux") {
    execFileSync("unzip", ["-q", archive, "-d", dir]);
  } else {
    execFileSync("tar", ["-xf", archive, "-C", dir]);
  }
  return dir;
}

function copy(from, to, mode) {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
  if (mode) fs.chmodSync(to, mode);
}

export function assemble({ version, archives, out }) {
  checkVersions(version);
  const sums = parseSums(fs.readFileSync(path.join(archives, "SHA256SUMS"), "utf8"));
  fs.rmSync(out, { recursive: true, force: true });
  const license = path.join(ROOT, "LICENSE");

  for (const [dir, target] of Object.entries(TARGETS)) {
    const name = archiveName(version, target);
    const file = path.join(archives, name);
    const want = sums.get(name);
    if (!want) throw new Error(`${name} is not in SHA256SUMS`);
    const got = sha256(file);
    if (got !== want) throw new Error(`${name}: SHA-256 ${got}, SHA256SUMS says ${want}`);

    const exe = target.includes("windows") ? "whatsup.exe" : "whatsup";
    const tmp = extract(file);
    try {
      const bin = path.join(tmp, `whatsup-v${version}-${target}`, exe);
      const dest = path.join(out, dir);
      copy(bin, path.join(dest, "bin", exe), 0o755);
      copy(path.join(PACKAGES, dir, "package.json"), path.join(dest, "package.json"));
      copy(path.join(PACKAGES, dir, "README.md"), path.join(dest, "README.md"));
      copy(license, path.join(dest, "LICENSE"));
    } finally {
      fs.rmSync(tmp, { recursive: true, force: true });
    }
  }

  const main = path.join(out, "whatsup-cli");
  copy(path.join(PACKAGES, "whatsup-cli", "bin", "whatsup.js"), path.join(main, "bin", "whatsup.js"), 0o755);
  copy(path.join(PACKAGES, "whatsup-cli", "package.json"), path.join(main, "package.json"));
  copy(path.join(PACKAGES, "whatsup-cli", "README.md"), path.join(main, "README.md"));
  copy(license, path.join(main, "LICENSE"));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: { version: { type: "string" }, archives: { type: "string" }, out: { type: "string" } },
  });
  if (!values.version || !values.archives || !values.out) {
    console.error("usage: node scripts/npm-assemble.mjs --version <x.y.z> --archives <dir> --out <dir>");
    process.exit(2);
  }
  try {
    assemble({ version: values.version.replace(/^v/, ""), archives: values.archives, out: values.out });
  } catch (e) {
    console.error(`npm-assemble: ${e.message}`);
    process.exit(1);
  }
  for (const dir of fs.readdirSync(values.out).sort()) console.log(`assembled ${path.join(values.out, dir)}`);
}
