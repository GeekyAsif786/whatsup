// Tests for bin/whatsup.js with a fake binary: a copy (Windows) or symlink
// of the running node executable, installed where npm would put the real
// platform package. Called with `-e <script>`, the "binary" runs the script,
// which shows what the launcher passed on.
//
//   node --test packages/whatsup-cli/test/launcher.test.js
"use strict";

const assert = require("node:assert/strict");
const { spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");

const { binaryPath, PACKAGES } = require("../bin/whatsup.js");

const LAUNCHER = path.join(__dirname, "..", "bin", "whatsup.js");
const PKG = PACKAGES[`${process.platform} ${process.arch}`];
const isWindows = process.platform === "win32";

const roots = [];
test.after(() => {
  for (const root of roots) fs.rmSync(root, { recursive: true, force: true });
});

/** A temp project with node_modules/whatsup-cli and, unless `withBinary`
 * is false, node_modules/<platform package>/bin/whatsup[.exe]. */
function install({ withBinary = true } = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "whatsup-npm-"));
  roots.push(root);
  const nm = path.join(root, "node_modules");
  const main = path.join(nm, "whatsup-cli");
  fs.mkdirSync(path.join(main, "bin"), { recursive: true });
  fs.copyFileSync(LAUNCHER, path.join(main, "bin", "whatsup.js"));
  fs.copyFileSync(path.join(__dirname, "..", "package.json"), path.join(main, "package.json"));
  if (withBinary && PKG) {
    const dir = path.join(nm, ...PKG.split("/"));
    fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
    fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: PKG, version: "0.0.0" }));
    const fake = path.join(dir, "bin", isWindows ? "whatsup.exe" : "whatsup");
    if (isWindows) fs.copyFileSync(process.execPath, fake);
    else fs.symlinkSync(process.execPath, fake);
  }
  return { root, launcher: path.join(main, "bin", "whatsup.js") };
}

test("maps every supported platform to its package", () => {
  assert.deepEqual(Object.values(PACKAGES).sort(), [
    "@whatsup-cli/darwin-arm64",
    "@whatsup-cli/darwin-x64",
    "@whatsup-cli/linux-arm64",
    "@whatsup-cli/linux-x64",
    "@whatsup-cli/win32-x64",
  ]);
  const deps = Object.keys(require("../package.json").optionalDependencies).sort();
  assert.deepEqual(deps, Object.values(PACKAGES).sort(), "optionalDependencies match");
});

test("resolves the binary inside the platform package", () => {
  const resolve = (id) => {
    assert.equal(id, "@whatsup-cli/linux-x64/package.json");
    return "/nm/@whatsup-cli/linux-x64/package.json";
  };
  assert.equal(binaryPath("linux", "x64", resolve), path.join("/nm/@whatsup-cli/linux-x64", "bin", "whatsup"));
  const win = binaryPath("win32", "x64", () => "C:/nm/@whatsup-cli/win32-x64/package.json");
  assert.equal(path.basename(win), "whatsup.exe");
});

test("unsupported platform: an error naming other install methods", () => {
  const err = binaryPath("freebsd", "x64", require.resolve);
  assert.ok(err instanceof Error);
  assert.match(err.message, /no binary for freebsd x64/);
  assert.match(err.message, /cargo install whatsup-cli/);
});

test("passes arguments through and returns the exit code", { skip: !PKG }, () => {
  const { launcher } = install();
  const script = "console.log(JSON.stringify(process.argv.slice(1))); process.exit(7)";
  const r = spawnSync(process.execPath, [launcher, "-e", script, "list", "--json", "a b"], {
    encoding: "utf8",
  });
  assert.equal(r.stderr, "");
  assert.deepEqual(JSON.parse(r.stdout), ["list", "--json", "a b"]);
  assert.equal(r.status, 7);
});

test("missing platform package: exit 1 with advice", { skip: !PKG }, () => {
  const { launcher } = install({ withBinary: false });
  const r = spawnSync(process.execPath, [launcher, "list"], { encoding: "utf8" });
  assert.equal(r.status, 1);
  assert.match(r.stderr, /is not installed/);
  assert.match(r.stderr, /--omit=optional/);
});

test("forwards SIGTERM to the binary", { skip: !PKG || isWindows }, async () => {
  const { launcher } = install();
  const script =
    "process.on('SIGTERM', () => { console.log('got SIGTERM'); process.exit(0); });" +
    "console.log('ready'); setInterval(() => {}, 1000);";
  const child = spawn(process.execPath, [launcher, "-e", script], { stdio: ["ignore", "pipe", "pipe"] });
  let out = "";
  const ready = new Promise((resolve) =>
    child.stdout.on("data", (d) => {
      out += d;
      if (out.includes("ready")) resolve();
    }),
  );
  await ready;
  child.kill("SIGTERM");
  const [code, signal] = await new Promise((resolve) => child.on("exit", (c, s) => resolve([c, s])));
  assert.match(out, /got SIGTERM/);
  assert.equal(signal, null);
  assert.equal(code, 0, "the binary's exit code, not death by signal");
});

test("a binary killed by a signal kills the launcher the same way", { skip: !PKG || isWindows }, () => {
  const { launcher } = install();
  const r = spawnSync(process.execPath, [launcher, "-e", "process.kill(process.pid, 'SIGKILL')"]);
  assert.equal(r.signal, "SIGKILL");
});
