#!/usr/bin/env node
// Launcher for the whatsup binary. npm installs exactly one of the
// optionalDependencies below (their package.json "os"/"cpu" fields pick the
// one for this machine); this script finds it and runs it with the same
// arguments, terminal and exit code. Nothing is downloaded at install time.
"use strict";

const { spawn } = require("node:child_process");
const path = require("node:path");

/** `${process.platform} ${process.arch}` → the package with that binary. */
const PACKAGES = {
  "darwin arm64": "@whatsup-cli/darwin-arm64",
  "darwin x64": "@whatsup-cli/darwin-x64",
  "linux arm64": "@whatsup-cli/linux-arm64",
  "linux x64": "@whatsup-cli/linux-x64",
  "win32 x64": "@whatsup-cli/win32-x64",
};

const OTHER_WAYS =
  "Other ways to install: `brew install GeekyAsif786/tap/whatsup`, " +
  "`cargo install whatsup-cli`, or a binary from " +
  "https://github.com/GeekyAsif786/whatsup/releases";

/**
 * The path of the whatsup binary for this platform, or an Error saying why
 * there is none. `resolve` is require.resolve (a parameter for tests).
 */
function binaryPath(platform, arch, resolve) {
  const pkg = PACKAGES[`${platform} ${arch}`];
  if (!pkg) {
    return new Error(`whatsup-cli has no binary for ${platform} ${arch}.\n${OTHER_WAYS}`);
  }
  let dir;
  try {
    dir = path.dirname(resolve(`${pkg}/package.json`));
  } catch {
    return new Error(
      `whatsup-cli: the package ${pkg} is not installed.\n` +
        "It is an optional dependency, so it is skipped by `--no-optional` / " +
        "`--omit=optional` and by lockfiles made on another platform. " +
        "Reinstall without those flags.\n" +
        OTHER_WAYS,
    );
  }
  return path.join(dir, "bin", platform === "win32" ? "whatsup.exe" : "whatsup");
}

function main() {
  const bin = binaryPath(process.platform, process.arch, require.resolve);
  if (bin instanceof Error) {
    console.error(bin.message);
    process.exit(1);
  }

  const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });

  // Ctrl-C reaches whatsup directly (same terminal process group); the
  // launcher only has to outlive it to pass on its exit code. SIGTERM and
  // SIGHUP sent to the launcher alone are forwarded, so whatsup can restore
  // the terminal.
  const forward = (signal) => () => {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  };
  const handlers = { SIGINT: () => {}, SIGTERM: forward("SIGTERM"), SIGHUP: forward("SIGHUP") };
  for (const [signal, handler] of Object.entries(handlers)) process.on(signal, handler);

  child.on("error", (err) => {
    console.error(`whatsup-cli: could not run ${bin}: ${err.message}\n${OTHER_WAYS}`);
    process.exit(1);
  });
  child.on("exit", (code, signal) => {
    if (signal) {
      // Die the same way, so the shell sees e.g. 143 for SIGTERM.
      for (const [s, handler] of Object.entries(handlers)) process.off(s, handler);
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 1);
  });
}

if (require.main === module) {
  main();
}

module.exports = { binaryPath, PACKAGES };
