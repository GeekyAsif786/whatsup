// node --test scripts/npm-assemble.test.mjs
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { TARGETS, archiveName, assemble, checkVersions, parseSums } from "./npm-assemble.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
const cargoVersion = fs.readFileSync(path.join(ROOT, "Cargo.toml"), "utf8").match(/^version = "(.+)"$/m)[1];

test("every npm package.json is at the Cargo.toml version", () => {
  checkVersions(cargoVersion);
});

test("checkVersions reports a stale package", () => {
  assert.throws(() => checkVersions("9.9.9"), /packages\/darwin-arm64: version .*, expected 9\.9\.9/);
});

test("parseSums reads text and binary-mode lines and rejects junk", () => {
  const a = "a".repeat(64);
  const b = "b".repeat(64);
  const sums = parseSums(`${a}  one.tar.gz\n${b} *two.zip\n`);
  assert.equal(sums.get("one.tar.gz"), a);
  assert.equal(sums.get("two.zip"), b);
  assert.throws(() => parseSums("nope  file\n"), /bad line/);
});

test("archive names match the release workflow", () => {
  assert.equal(archiveName("0.1.0", "aarch64-apple-darwin"), "whatsup-v0.1.0-aarch64-apple-darwin.tar.gz");
  assert.equal(archiveName("0.1.0", "x86_64-pc-windows-msvc"), "whatsup-v0.1.0-x86_64-pc-windows-msvc.zip");
});

/** Five fake archives laid out like the release workflow's, plus SHA256SUMS. */
function fakeRelease(version) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "whatsup-fake-release-"));
  const lines = [];
  for (const target of Object.values(TARGETS)) {
    const name = `whatsup-v${version}-${target}`;
    const exe = target.includes("windows") ? "whatsup.exe" : "whatsup";
    fs.mkdirSync(path.join(dir, name));
    fs.writeFileSync(path.join(dir, name, exe), `fake ${target}`);
    const archive = archiveName(version, target);
    if (archive.endsWith(".zip") && process.platform === "linux") {
      execFileSync("zip", ["-qr", archive, name], { cwd: dir });
    } else if (archive.endsWith(".zip")) {
      execFileSync("tar", ["-a", "-cf", archive, name], { cwd: dir });
    } else {
      execFileSync("tar", ["-czf", archive, name], { cwd: dir });
    }
    const hash = createHash("sha256").update(fs.readFileSync(path.join(dir, archive))).digest("hex");
    lines.push(`${hash}  ${archive}`);
  }
  fs.writeFileSync(path.join(dir, "SHA256SUMS"), `${lines.join("\n")}\n`);
  return dir;
}

test("assembles one package per platform plus the launcher package", () => {
  const archives = fakeRelease(cargoVersion);
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "whatsup-npm-out-"));
  try {
    assemble({ version: cargoVersion, archives, out });
    assert.deepEqual(fs.readdirSync(out).sort(), [...Object.keys(TARGETS), "whatsup-cli"].sort());
    for (const [dir, target] of Object.entries(TARGETS)) {
      const exe = target.includes("windows") ? "whatsup.exe" : "whatsup";
      assert.deepEqual(fs.readdirSync(path.join(out, dir)).sort(), ["LICENSE", "README.md", "bin", "package.json"]);
      assert.equal(fs.readFileSync(path.join(out, dir, "bin", exe), "utf8"), `fake ${target}`);
      if (process.platform !== "win32") {
        assert.equal(fs.statSync(path.join(out, dir, "bin", exe)).mode & 0o777, 0o755);
      }
    }
    assert.ok(fs.existsSync(path.join(out, "whatsup-cli", "bin", "whatsup.js")));
  } finally {
    fs.rmSync(archives, { recursive: true, force: true });
    fs.rmSync(out, { recursive: true, force: true });
  }
});

test("refuses an archive whose checksum doesn't match", () => {
  const archives = fakeRelease(cargoVersion);
  const out = fs.mkdtempSync(path.join(os.tmpdir(), "whatsup-npm-out-"));
  try {
    const victim = path.join(archives, archiveName(cargoVersion, TARGETS["linux-x64"]));
    fs.appendFileSync(victim, "tampered");
    assert.throws(() => assemble({ version: cargoVersion, archives, out }), /linux-gnu\.tar\.gz: SHA-256 .* SHA256SUMS says/);
  } finally {
    fs.rmSync(archives, { recursive: true, force: true });
    fs.rmSync(out, { recursive: true, force: true });
  }
});
