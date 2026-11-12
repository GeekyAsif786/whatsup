import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { LABELS_PATH, formatPlan, ghCommands, parseLabels, planLabels } from "./sync-labels.mjs";

const REQUIRED = [
  "good first issue", "help wanted", "needs-triage", "framework-rule", "bug", "enhancement",
  "platform: linux", "platform: macos", "platform: windows", "docker", "tui", "packaging",
  "performance", "documentation", "wontfix", "duplicate", "dependencies", "rust", "github_actions",
];

test("labels.yml parses and holds exactly the agreed labels", async () => {
  const labels = parseLabels(await readFile(LABELS_PATH, "utf8"));
  assert.deepEqual(labels.map((l) => l.name), REQUIRED);
  for (const l of labels) {
    assert.match(l.color, /^[0-9a-f]{6}$/, l.name);
    assert.ok(l.description.length > 0 && l.description.length <= 100, l.name);
  }
  assert.equal(labels.find((l) => l.name === "github_actions").color, "000000");
  assert.equal(labels.find((l) => l.name === "platform: macos").name, "platform: macos");
});

test("parser: comments, blank lines, quotes, case-folded colours", () => {
  const text = `# header
- name: "needs triage"   # trailing comment
  color: 'ABCDEF'
  description: Something isn't working

- name: plain
  color: "000000"
  description: x
`;
  assert.deepEqual(parseLabels(text), [
    { name: "needs triage", color: "abcdef", description: "Something isn't working" },
    { name: "plain", color: "000000", description: "x" },
  ]);
});

test("parser rejects malformed input with a line number", () => {
  assert.throws(() => parseLabels("name: x\n"), /labels\.yml:1: expected/);
  assert.throws(() => parseLabels("  name: x\n"), /property outside a list item/);
  assert.throws(() => parseLabels("- name: x\n  colour: ffffff\n"), /labels\.yml:2: unknown key "colour"/);
  assert.throws(() => parseLabels("- name: x\n  name: y\n"), /duplicate key "name"/);
  assert.throws(() => parseLabels("- name: x\n    color: ffffff\n"), /labels\.yml:2: expected/);
});

test("parser validates each label", () => {
  const one = (color, description = "d") => `- name: x\n  color: ${color}\n  description: ${description}\n`;
  assert.throws(() => parseLabels("- name: x\n  color: ffffff\n"), /missing description/);
  assert.throws(() => parseLabels(one('"#ffffff"')), /color must be 6 hex digits/);
  assert.throws(() => parseLabels(one("fff")), /color must be 6 hex digits/);
  assert.throws(() => parseLabels(one("ffffff", "a".repeat(101))), /description over 100/);
  assert.throws(() => parseLabels(one("ffffff") + "- name: X\n  color: 000000\n  description: d\n"), /label 2 \("X"\): duplicate name/);
});

test("plan: create, update (colour, description, case of name), unchanged, untracked", () => {
  const wanted = [
    { name: "preset", color: "1d4ed8", description: "Presets" },
    { name: "bug", color: "d73a4a", description: "Broken" },
    { name: "Docs", color: "0075ca", description: "Docs" },
    { name: "wontfix", color: "ffffff", description: "No" },
  ];
  const existing = [
    { name: "bug", color: "D73A4A", description: "Something isn't working" },
    { name: "docs", color: "0075CA", description: "Docs" },
    { name: "wontfix", color: "FFFFFF", description: "No" },
    { name: "question", color: "d876e3", description: "?" },
  ];
  const plan = planLabels(wanted, existing);
  assert.deepEqual(plan.create.map((l) => l.name), ["preset"]);
  assert.deepEqual(plan.update.map((l) => [l.from.name, l.name]), [["bug", "bug"], ["docs", "Docs"]]);
  assert.deepEqual(plan.unchanged, ["wontfix"]);
  assert.deepEqual(plan.untracked, ["question"]);

  assert.deepEqual(ghCommands(plan, "o/r"), [
    ["label", "create", "preset", "--color", "1d4ed8", "--description", "Presets", "--repo", "o/r"],
    ["label", "edit", "bug", "--color", "d73a4a", "--description", "Broken", "--repo", "o/r"],
    ["label", "edit", "docs", "--name", "Docs", "--color", "0075ca", "--description", "Docs", "--repo", "o/r"],
  ]);
  assert.deepEqual(ghCommands(plan)[0], ["label", "create", "preset", "--color", "1d4ed8", "--description", "Presets"]);

  const text = formatPlan(plan);
  assert.match(text, /^create {5}preset {2}#1d4ed8 {2}Presets$/m);
  assert.match(text, /^update {5}bug {2}description "Something isn't working" → "Broken"$/m);
  assert.match(text, /^update {5}Docs {2}name "docs" → "Docs"$/m);
  assert.match(text, /^unchanged {2}wontfix$/m);
  assert.match(text, /^untracked {2}question {2}\(not in labels\.yml; left alone\)$/m);
});

test("the sync never deletes: no delete command is ever planned", () => {
  const plan = planLabels([], [{ name: "a", color: "ffffff", description: "" }]);
  assert.deepEqual(ghCommands(plan), []);
  assert.deepEqual(plan.untracked, ["a"]);
});
