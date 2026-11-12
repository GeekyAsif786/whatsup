// Sync GitHub labels from .github/labels.yml using the gh CLI.
//   node scripts/sync-labels.mjs            dry run: print the plan
//   node scripts/sync-labels.mjs --apply    create/update labels
//   --repo owner/name                       target another repo (default: gh's current repo)
// Creates and updates only; never deletes. Run by the maintainer by hand,
// never in CI (CI must not hold a token that can write labels).
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const ROOT = fileURLToPath(new URL("..", import.meta.url));
export const LABELS_PATH = join(ROOT, ".github", "labels.yml");
const KEYS = new Set(["name", "color", "description"]);
const MAX_DESCRIPTION = 100; // GitHub's limit

const unquote = (v) => {
  const m = v.match(/^"(.*)"$/) ?? v.match(/^'(.*)'$/);
  return m ? m[1] : v;
};

/**
 * Parse the restricted YAML of labels.yml: a top-level list of flat maps
 * whose values are plain or quoted scalars. Throws on anything else.
 *
 * @param {string} text
 * @returns {{ name: string, color: string, description: string }[]}
 */
export function parseLabels(text) {
  const labels = [];
  let current = null;
  text.split(/\r?\n/).forEach((raw, i) => {
    const line = raw.replace(/\s+#.*$/, "").replace(/^#.*$/, "").trimEnd();
    if (!line.trim()) return;
    const m = line.match(/^(- | {2})([a-z_]+):\s*(.*)$/);
    if (!m) throw new Error(`labels.yml:${i + 1}: expected "- key: value" or "  key: value", got ${JSON.stringify(raw)}`);
    const [, lead, key, value] = m;
    if (lead === "- ") labels.push((current = {}));
    else if (!current) throw new Error(`labels.yml:${i + 1}: property outside a list item`);
    if (!KEYS.has(key)) throw new Error(`labels.yml:${i + 1}: unknown key "${key}"`);
    if (key in current) throw new Error(`labels.yml:${i + 1}: duplicate key "${key}"`);
    current[key] = unquote(value.trim());
  });

  const seen = new Set();
  labels.forEach((l, i) => {
    const where = `labels.yml: label ${i + 1}${l.name ? ` ("${l.name}")` : ""}`;
    for (const k of KEYS) if (!l[k]) throw new Error(`${where}: missing ${k}`);
    if (!/^[0-9a-f]{6}$/i.test(l.color)) throw new Error(`${where}: color must be 6 hex digits without "#"`);
    if (l.description.length > MAX_DESCRIPTION) throw new Error(`${where}: description over ${MAX_DESCRIPTION} characters`);
    const key = l.name.toLowerCase();
    if (seen.has(key)) throw new Error(`${where}: duplicate name`);
    seen.add(key);
    l.color = l.color.toLowerCase();
  });
  return labels;
}

/**
 * Compare wanted labels with the repo's labels (names match case-insensitively).
 *
 * @param {{ name: string, color: string, description: string }[]} wanted
 * @param {{ name: string, color: string, description?: string }[]} existing
 * @returns {{ create: object[], update: { name: string, color: string, description: string, from: object }[], unchanged: string[], untracked: string[] }}
 */
export function planLabels(wanted, existing) {
  const byName = new Map(existing.map((l) => [l.name.toLowerCase(), l]));
  const plan = { create: [], update: [], unchanged: [], untracked: [] };
  for (const w of wanted) {
    const e = byName.get(w.name.toLowerCase());
    if (!e) plan.create.push(w);
    else if (e.name !== w.name || e.color.toLowerCase() !== w.color || (e.description ?? "") !== w.description) {
      plan.update.push({ ...w, from: e });
    } else plan.unchanged.push(w.name);
  }
  const wantedNames = new Set(wanted.map((w) => w.name.toLowerCase()));
  plan.untracked = existing.filter((e) => !wantedNames.has(e.name.toLowerCase())).map((e) => e.name);
  return plan;
}

/**
 * The gh argument lists that carry out `plan`.
 * @returns {string[][]}
 */
export function ghCommands(plan, repo) {
  const r = repo ? ["--repo", repo] : [];
  return [
    ...plan.create.map((l) => ["label", "create", l.name, "--color", l.color, "--description", l.description, ...r]),
    ...plan.update.map((l) => [
      "label", "edit", l.from.name,
      ...(l.from.name !== l.name ? ["--name", l.name] : []),
      "--color", l.color, "--description", l.description, ...r,
    ]),
  ];
}

/** Human-readable plan, one line per label. */
export function formatPlan(plan) {
  const lines = [];
  for (const l of plan.create) lines.push(`create     ${l.name}  #${l.color}  ${l.description}`);
  for (const l of plan.update) {
    const changes = [];
    if (l.from.name !== l.name) changes.push(`name "${l.from.name}" → "${l.name}"`);
    if (l.from.color.toLowerCase() !== l.color) changes.push(`color #${l.from.color.toLowerCase()} → #${l.color}`);
    if ((l.from.description ?? "") !== l.description) changes.push(`description "${l.from.description ?? ""}" → "${l.description}"`);
    lines.push(`update     ${l.name}  ${changes.join("; ")}`);
  }
  for (const n of plan.unchanged) lines.push(`unchanged  ${n}`);
  for (const n of plan.untracked) lines.push(`untracked  ${n}  (not in labels.yml; left alone)`);
  return lines.join("\n");
}

async function main() {
  const args = process.argv.slice(2);
  const apply = args.includes("--apply");
  const repoAt = args.indexOf("--repo");
  const repo = repoAt >= 0 ? args[repoAt + 1] : undefined;
  const gh = promisify(execFile);

  const wanted = parseLabels(await readFile(LABELS_PATH, "utf8"));
  const list = ["label", "list", "--limit", "500", "--json", "name,color,description", ...(repo ? ["--repo", repo] : [])];
  const existing = JSON.parse((await gh("gh", list)).stdout);
  const plan = planLabels(wanted, existing);
  console.log(formatPlan(plan));
  const commands = ghCommands(plan, repo);
  if (!apply) {
    console.log(`\nDry run: ${commands.length} change(s). Re-run with --apply to make them.`);
    return;
  }
  for (const c of commands) {
    await gh("gh", c);
    console.log(`done: gh ${c.slice(0, 3).join(" ")}`);
  }
  console.log(`\nApplied ${commands.length} change(s).`);
}

if (process.argv[1] && basename(process.argv[1]) === basename(fileURLToPath(import.meta.url))) {
  await main();
}
