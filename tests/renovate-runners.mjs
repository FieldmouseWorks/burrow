// renovate-runners RENOVATE CONFIG: what Renovate does under CONFIG with a
// workflow whose job runs on an older GitHub-hosted runner (`runs-on:
// ubuntu-22.04`). Renovate's own code extracts it, looks it up in its
// github-runners datasource (a list Renovate ships, with no release dates)
// and branches the update, as a run does, then processes each branch twice:
// as a branch it would create, and as one whose PR is already open on that
// update. One line per branch: the branch, what processing does with it new
// (pending, or creates it) and open (pending, or the stability check it
// sets), its title, and each update in it.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [renovate, file] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { getConfig } = await import(`${dist}/config/defaults.js`);
const { mergeChildConfig } = await import(`${dist}/config/utils.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { mergeRenovateConfig } = await import(`${dist}/workers/repository/init/merge.js`);
const { extractPackageFile } = await import(`${dist}/modules/manager/index.js`);
const { fetchUpdates } = await import(`${dist}/workers/repository/process/fetch.js`);
const { branchifyUpgrades } = await import(`${dist}/workers/repository/updates/branchify.js`);
const { processBranch } = await import(`${dist}/workers/repository/update/branch/index.js`);
const { default: platforms } = await import(`${dist}/modules/platform/api.js`);
const { platformScmImpls } = await import(`${dist}/modules/platform/scm.js`);
const { setPlatformApi } = await import(`${dist}/modules/platform/index.js`);

// The platform and the repo's git, as processing a branch sees them: the
// branch absent, or present with its PR open and already on the update.
// Processing a new branch stops where it would check out the base to write
// the branch; an open one records the status checks it sets.
let exists = false;
let creates = false;
let statuses = {};
const pr = { number: 1, state: "open", labels: [], targetBranch: "main", bodyStruct: {} };
const answers = {
  getBranchPr: async () => (exists ? pr : null),
  findPr: async () => null,
  getBranchStatusCheck: async () => null,
  setBranchStatus: async ({ context, state }) => { statuses[context] = state; },
  refreshPr: async () => {},
};
platforms.set("fake", new Proxy(answers, { get: (target, name) => target[name] }));
const repoGit = {
  branchExists: async () => exists,
  isBranchBehindBase: async () => false,
  isBranchModified: async () => false,
  isBranchConflicted: async () => false,
  checkoutBranch: async () => { creates = true; throw new Error("stop: the branch would be written"); },
};
platformScmImpls.set("fake", class { constructor() { return repoGit; } });
setPlatformApi("fake");

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-runners-"));
GlobalConfig.set({ requireConfig: "ignored", localDir: dir, cacheDir: path.join(dir, ".cache") });
const name = ".github/workflows/release.yml";
const workflow = `on: push
jobs:
  build:
    runs-on: ubuntu-22.04
    steps:
      - run: make
`;
fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
fs.writeFileSync(path.join(dir, name), workflow);

const config = await mergeRenovateConfig(mergeChildConfig(getConfig(), JSON.parse(fs.readFileSync(file, "utf8"))));
Object.assign(config, { repository: "sam/other", baseBranch: "main", repoIsOnboarded: true, errors: [], warnings: [] });
const outcome = async (branch, open) => {
  exists = open;
  creates = false;
  statuses = {};
  let result;
  try {
    ({ result } = await processBranch(open ? { ...branch, cacheFingerprintMatch: "matched" } : branch));
  } catch {
    // Past the point this looks at.
  }
  if (result === "pending") return "pending";
  if (!open) return creates ? "creates" : `${result}`;
  return `stability ${statuses["renovate/stability-days"] ?? "none"}`;
};

const manager = "github-actions";
const res = await extractPackageFile(manager, workflow, name, mergeChildConfig(config, { manager, ...config[manager] }));
const packageFiles = { [manager]: res ? [{ packageFile: name, ...res }] : [] };
await fetchUpdates(config, packageFiles);
const { branches } = await branchifyUpgrades(config, packageFiles);
for (const b of branches.sort((x, y) => x.branchName.localeCompare(y.branchName))) {
  const updates = b.upgrades.map((u) => `${u.datasource} ${u.depName} ${u.updateType} ${u.newValue}`).sort().join(", ");
  console.log(`${b.branchName} new: ${await outcome(b, false)}, open: ${await outcome(b, true)}: ${b.prTitle}: ${updates}`);
}
