// Community files: issue forms, PR template, release-note categories.
// Text checks (no YAML parser); every label used must be in .github/labels.yml.
//   node --test scripts/community.test.mjs
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import { test } from "node:test";

import { LABELS_PATH, parseLabels } from "./sync-labels.mjs";

const gh = (p) => new URL(`../.github/${p}`, import.meta.url);
const read = (p) => readFile(gh(p), "utf8");
const known = async () => new Set(parseLabels(await readFile(LABELS_PATH, "utf8")).map((l) => l.name));
/** `labels: [a, b]` → ["a", "b"] (every such line in the file). */
const labelsIn = (text) =>
  [...text.matchAll(/labels: \[(.*)\]$/gm)].flatMap((m) =>
    m[1].split(",").map((s) => s.trim().replace(/^"|"$/g, "")).filter(Boolean),
  );
const ids = (text) => [...text.matchAll(/^ {4}id: (\S+)$/gm)].map((m) => m[1]);

test("issue forms: the agreed set, labels from labels.yml, unique field ids", async () => {
  const labels = await known();
  assert.deepEqual((await readdir(gh("ISSUE_TEMPLATE/"))).sort(), ["bug.yml", "config.yml", "feature.yml", "framework-rule.yml"]);
  const expected = {
    "bug.yml": ["bug", "needs-triage"],
    "feature.yml": ["enhancement", "needs-triage"],
    "framework-rule.yml": ["framework-rule"],
  };
  for (const [name, want] of Object.entries(expected)) {
    const text = await read(`ISSUE_TEMPLATE/${name}`);
    assert.match(text, /^name: .+\ndescription: .+\n/, name);
    assert.deepEqual(labelsIn(text), want, name);
    for (const l of want) assert.ok(labels.has(l), `${name}: unknown label ${l}`);
    const fieldIds = ids(text);
    assert.ok(fieldIds.length > 0, name);
    assert.equal(new Set(fieldIds).size, fieldIds.length, `${name}: duplicate ids`);
  }
});

test("blank issues are off and security goes to the private policy", async () => {
  const text = await read("ISSUE_TEMPLATE/config.yml");
  assert.match(text, /^blank_issues_enabled: false$/m);
  assert.match(text, /security\/policy/);
});

test("the framework form links the CONTRIBUTING walkthrough that exists", async () => {
  const form = await read("ISSUE_TEMPLATE/framework-rule.yml");
  assert.match(form, /CONTRIBUTING\.md#add-a-framework-rule/);
  const contributing = await readFile(new URL("../CONTRIBUTING.md", import.meta.url), "utf8");
  assert.match(contributing, /^## Add a framework rule$/m);
});

test("release-note categories use known labels and end with the catch-all", async () => {
  const labels = await known();
  const text = await read("release.yml");
  const used = labelsIn(text).filter((l) => l !== "*");
  for (const l of used) assert.ok(labels.has(l), `release.yml: unknown label ${l}`);
  const titles = [...text.matchAll(/- title: (.+)$/gm)].map((m) => m[1]);
  assert.equal(titles.at(-1), "Other changes");
  assert.match(text, /labels: \["\*"\]\s*$/);
});

test("PR template has the checks CI runs", async () => {
  const text = await read("PULL_REQUEST_TEMPLATE.md");
  assert.match(text, /cargo clippy --all-targets --locked -- -D warnings/);
  assert.match(text, /cargo test --locked/);
});
