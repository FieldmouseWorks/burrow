// renovate-follow CLOCK RENOVATE CONFIG: what Renovate does under CONFIG for a
// project that follows a homelab repo, its trusted owner's (sam/homelab)
// or someone else's (kim/homelab): its workflows and action pinned to
// homelab's main (one not pinned to a commit yet), and its flake input
// pinned there by commit (`?ref=main&rev=...`, at either spelling of the
// repo's URL) or not at all, beside two nixpkgs inputs on their channels.
// Renovate's own code extracts them, looks them up (homelab's main stood
// in for, its head a given number of minutes old) and branches the
// updates, as a run does,
// then processes each branch twice: as a branch it would create, and as
// one whose PR is already open on that update. Each flake's nix inputs
// first, with the versioning their lookup used; then one line per branch
// but the lock file refreshes (a refresh leaves an input pinned by commit
// where it is): how the flake pins homelab, the head's age, the branch,
// what processing does with it new (pending, or creates it) and open
// (pending, or the stability check it sets), its title, the labels its
// PR gets, and each update in it. On CLOCK's night, in the policy's
// window, and then at noon, outside it.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [clock, renovate, file] = process.argv.slice(2);
const { at } = await import(clock);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { getConfig } = await import(`${dist}/config/defaults.js`);
const { mergeChildConfig } = await import(`${dist}/config/utils.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { mergeRenovateConfig } = await import(`${dist}/workers/repository/init/merge.js`);
const { extractPackageFile } = await import(`${dist}/modules/manager/index.js`);
const { fetchUpdates } = await import(`${dist}/workers/repository/process/fetch.js`);
const { branchifyUpgrades } = await import(`${dist}/workers/repository/updates/branchify.js`);
const { processBranch } = await import(`${dist}/workers/repository/update/branch/index.js`);
const { prepareLabels } = await import(`${dist}/workers/repository/update/pr/labels.js`);
const { default: platforms } = await import(`${dist}/modules/platform/api.js`);
const { platformScmImpls } = await import(`${dist}/modules/platform/scm.js`);
const { setPlatformApi } = await import(`${dist}/modules/platform/index.js`);
const { GithubDigestDatasource } = await import(`${dist}/modules/datasource/github-digest/index.js`);
const { GitRefsDatasource } = await import(`${dist}/modules/datasource/git-refs/index.js`);

const OLD = "1111111111111111111111111111111111111111";
const NEW = "2222222222222222222222222222222222222222";
let age = 0;
// homelab's main, on GitHub as OWNER/NAME (with its head's commit date) and
// through git by its URL (without one). Nothing else answers.
const github = (name) => /^(sam|kim)\/homelab$/.test(name);
const git = (name) => /^https:\/\/github\.com\/(sam|kim)\/homelab(\.git)?$/.test(name);
const sourceUrl = (name) => `https://github.com/${name.match(/(sam|kim)\/homelab/)[0]}`;
GithubDigestDatasource.prototype.getReleases = async ({ packageName }) => github(packageName)
  ? { sourceUrl: sourceUrl(packageName), releases: [
    { version: "main", gitRef: "main", newDigest: NEW, releaseTimestamp: new Date(Date.now() - age * 60_000).toISOString() },
  ] }
  : null;
GithubDigestDatasource.prototype.getDigest = async ({ packageName }, value) =>
  github(packageName) && value === "main" ? NEW : null;
GitRefsDatasource.prototype.getReleases = async ({ packageName }) => git(packageName)
  ? { sourceUrl: sourceUrl(packageName), releases: [{ version: "main", gitRef: "main", newDigest: NEW }] }
  : null;
GitRefsDatasource.prototype.getDigest = async ({ packageName }, value) =>
  git(packageName) && value === "main" ? NEW : null;

// The platform and the repo's git, as processing a branch sees them: the
// branch absent, or present with its PR open and already on the update.
// Processing a new branch stops where it would check out the base to
// write the branch; an open one records the status checks it sets.
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

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-follow-"));
GlobalConfig.set({ requireConfig: "ignored", localDir: dir, cacheDir: path.join(dir, ".cache") });
const workflow = (owner) => `on: push
jobs:
  check:
    uses: ${owner}/homelab/.github/workflows/flake-check.yml@${OLD} # main
  lint:
    uses: ${owner}/homelab/.github/workflows/lint.yml@main
  house:
    runs-on: ubuntu-latest
    steps:
      - uses: ${owner}/homelab/.github/actions/house@${OLD} # main
`;
// The flake, its homelab input at REPO's URL pinned by commit or not,
// nixpkgs on a stable channel pinned by commit and on unstable not, and
// the lock nix writes for it.
const narHash = "sha256-V062oHtqsndvwU2wlks6DGDsU3cMx/gc5d8TAgTyr6Y=";
const nixpkgs = (ref, rev) => ({
  locked: { lastModified: 1793794403, narHash, owner: "NixOS", repo: "nixpkgs", rev: OLD, type: "github" },
  original: { owner: "NixOS", ref, repo: "nixpkgs", type: "github", ...(rev ? { rev: OLD } : {}) },
});
const flake = (repo, pinned) => {
  const original = { type: "git", url: repo, ...(pinned ? { ref: "main", rev: OLD } : {}) };
  const url = pinned ? `git+${repo}?ref=main&rev=${OLD}` : `git+${repo}`;
  return {
    "flake.nix": `{
  inputs.homelab.url = "${url}";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11?rev=${OLD}";
  inputs.unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, homelab, ... }: homelab.lib.project { inherit self; };
}
`,
    "flake.lock": JSON.stringify({ version: 7, root: "root", nodes: {
      root: { inputs: { homelab: "homelab", nixpkgs: "nixpkgs", unstable: "unstable" } },
      homelab: {
        locked: { lastModified: 1793794403, narHash, ref: "main", rev: OLD, revCount: 409, type: "git", url: repo },
        original,
      },
      nixpkgs: nixpkgs("nixos-25.11", true),
      unstable: nixpkgs("nixos-unstable", false),
    } }),
  };
};

const config = await mergeRenovateConfig(mergeChildConfig(getConfig(), JSON.parse(fs.readFileSync(file, "utf8"))));
Object.assign(config, { repository: "sam/project", baseBranch: "main", repoIsOnboarded: true, errors: [], warnings: [] });
// What processing BRANCH does with it new, or open on its update.
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
  if (["pending", "not-scheduled", "update-not-scheduled"].includes(result)) return result;
  if (!open) return creates ? "creates" : `${result}`;
  return `stability ${statuses["renovate/stability-days"] ?? "none"}`;
};
// Someone else's repo soaks three days; the trusted owner's batches half
// an hour. At noon, out of the window, neither branch is made, and an open
// one is merged but the trusted owner's isn't moved.
const NIGHT = "2026-11-06T01:30:00Z";
const NOON = "2026-11-06T12:00:00Z";
const flakes = [
  ["pinned", "sam", "https://github.com/sam/homelab", true, [10, 40], NIGHT],
  ["pinned .git", "sam", "https://GitHub.com/sam/homelab.git", true, [10, 40], NIGHT],
  ["unpinned", "sam", "https://github.com/sam/homelab", false, [10, 40], NIGHT],
  ["someone else's, pinned", "kim", "https://github.com/kim/homelab", true, [40, 4400], NIGHT],
  ["pinned at noon", "sam", "https://github.com/sam/homelab", true, [40], NOON],
  ["someone else's, pinned at noon", "kim", "https://github.com/kim/homelab", true, [4400], NOON],
];
for (const [label, owner, repo, pinned, ages, when] of flakes) {
  at(when);
  const files = { ".github/workflows/flake.yml": workflow(owner), ...flake(repo, pinned) };
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), text);
  }
  for (const minutes of ages) {
    age = minutes;
    const packageFiles = {};
    for (const [manager, name] of [["github-actions", ".github/workflows/flake.yml"], ["nix", "flake.nix"]]) {
      // The manager as a run has it, nix (off by Renovate's default) on as this config has it.
      const res = await extractPackageFile(manager, files[name], name, mergeChildConfig(config, { manager, ...config[manager] }));
      packageFiles[manager] = res ? [{ packageFile: name, ...res }] : [];
    }
    await fetchUpdates(config, packageFiles);
    if (minutes === ages[0]) {
      const inputs = packageFiles.nix[0].deps.map((d) => `${d.depName} ${d.versioning}`).join(", ");
      console.log(`${label}: nix ${inputs}`);
    }
    const { branches } = await branchifyUpgrades(config, packageFiles);
    for (const b of branches.filter((b) => !b.isLockFileMaintenance).sort((x, y) => x.branchName.localeCompare(y.branchName))) {
      const updates = b.upgrades.map((u) => `${u.manager} ${u.depName} ${u.updateType} ${u.newDigest.slice(0, 7)}`).sort().join(", ");
      console.log(`${label} ${minutes} min: ${b.branchName} new: ${await outcome(b, false)}, open: ${await outcome(b, true)}: ${b.prTitle} [${prepareLabels(b).join(", ")}]: ${updates}`);
    }
  }
}
// A project whose own dependencies Dependabot keeps (the example's
// followOnly, sam/kept): with both homelabs' workflows and a flake pinned to
// the trusted owner's, only that owner's branch pins come up, in their one
// batch; someone else's pins, their commit pin and every lock file refresh
// stay off. Every branch is listed here, refreshes included.
{
  Object.assign(config, { repository: "sam/kept" });
  const files = {
    ".github/workflows/flake.yml": workflow("sam"),
    ".github/workflows/other.yml": workflow("kim"),
    ...flake("https://github.com/sam/homelab", true),
  };
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), text);
  }
  at(NIGHT);
  age = 40;
  const packageFiles = {};
  for (const [manager, names] of [["github-actions", [".github/workflows/flake.yml", ".github/workflows/other.yml"]], ["nix", ["flake.nix"]]]) {
    packageFiles[manager] = [];
    for (const name of names) {
      const res = await extractPackageFile(manager, files[name], name, mergeChildConfig(config, { manager, ...config[manager] }));
      if (res) packageFiles[manager].push({ packageFile: name, ...res });
    }
  }
  await fetchUpdates(config, packageFiles);
  const { branches } = await branchifyUpgrades(config, packageFiles);
  for (const b of branches.sort((x, y) => x.branchName.localeCompare(y.branchName))) {
    const updates = b.upgrades.map((u) => `${u.manager} ${u.depName} ${u.updateType} ${(u.newDigest ?? "").slice(0, 7)}`).sort().join(", ");
    console.log(`kept 40 min: ${b.branchName}: ${b.prTitle}: ${updates}`);
  }
}
