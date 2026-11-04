// renovate-follow RENOVATE CONFIG: the branches Renovate opens under CONFIG
// for a project that follows a repo of its trusted owner's (sam/homelab):
// its workflow and action pinned to homelab's main, and its flake input
// pinned there by commit (`?ref=main&rev=...`, at either spelling of the
// repo's URL) or not at all. Renovate's own
// code extracts them, looks them up (homelab's main stood in for, its head a
// given number of minutes old) and branches the updates, as a run does. One
// line per branch but the lock file refreshes (a refresh leaves an input
// pinned by commit where it is): how the flake pins homelab, the head's age,
// the branch, whether it waits (pending) or opens, its title, and each
// update in it.
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
const { GithubDigestDatasource } = await import(`${dist}/modules/datasource/github-digest/index.js`);
const { GitRefsDatasource } = await import(`${dist}/modules/datasource/git-refs/index.js`);

const OLD = "1111111111111111111111111111111111111111";
const NEW = "2222222222222222222222222222222222222222";
let age = 0;
// homelab's main, on GitHub as OWNER/NAME (with its head's commit date) and
// through git by its URL (without one). Nothing else answers.
const github = (name) => name === "sam/homelab";
const git = (name) => /^https:\/\/github\.com\/sam\/homelab(\.git)?$/.test(name);
GithubDigestDatasource.prototype.getReleases = async ({ packageName }) => github(packageName)
  ? { sourceUrl: "https://github.com/sam/homelab", releases: [
    { version: "main", gitRef: "main", newDigest: NEW, releaseTimestamp: new Date(Date.now() - age * 60_000).toISOString() },
  ] }
  : null;
GithubDigestDatasource.prototype.getDigest = async ({ packageName }, value) =>
  github(packageName) && value === "main" ? NEW : null;
GitRefsDatasource.prototype.getReleases = async ({ packageName }) => git(packageName)
  ? { sourceUrl: "https://github.com/sam/homelab", releases: [{ version: "main", gitRef: "main", newDigest: NEW }] }
  : null;
GitRefsDatasource.prototype.getDigest = async ({ packageName }, value) =>
  git(packageName) && value === "main" ? NEW : null;

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-follow-"));
GlobalConfig.set({ requireConfig: "ignored", localDir: dir, cacheDir: path.join(dir, ".cache") });
const workflow = `on: push
jobs:
  check:
    uses: sam/homelab/.github/workflows/flake-check.yml@${OLD} # main
  house:
    runs-on: ubuntu-latest
    steps:
      - uses: sam/homelab/.github/actions/house@${OLD} # main
`;
// The flake, its input at REPO's URL pinned by commit or not, and the lock
// nix writes for it.
const flake = (repo, pinned) => {
  const original = { type: "git", url: repo, ...(pinned ? { ref: "main", rev: OLD } : {}) };
  const url = pinned ? `git+${repo}?ref=main&rev=${OLD}` : `git+${repo}`;
  return {
    "flake.nix": `{\n  inputs.homelab.url = "${url}";\n  outputs = { self, homelab }: homelab.lib.project { inherit self; };\n}\n`,
    "flake.lock": JSON.stringify({ version: 7, root: "root", nodes: {
      root: { inputs: { homelab: "homelab" } },
      homelab: {
        locked: { lastModified: 1793794403, narHash: "sha256-V062oHtqsndvwU2wlks6DGDsU3cMx/gc5d8TAgTyr6Y=", ref: "main", rev: OLD, revCount: 409, type: "git", url: repo },
        original,
      },
    } }),
  };
};

const config = await mergeRenovateConfig(mergeChildConfig(getConfig(), JSON.parse(fs.readFileSync(file, "utf8"))));
Object.assign(config, { repository: "sam/project", baseBranch: "main", repoIsOnboarded: true, errors: [], warnings: [] });
const flakes = [
  ["pinned", "https://github.com/sam/homelab", true],
  ["pinned .git", "https://GitHub.com/sam/homelab.git", true],
  ["unpinned", "https://github.com/sam/homelab", false],
];
for (const [label, repo, pinned] of flakes) {
  const files = { ".github/workflows/flake.yml": workflow, ...flake(repo, pinned) };
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(dir, name)), { recursive: true });
    fs.writeFileSync(path.join(dir, name), text);
  }
  for (const minutes of [10, 40]) {
    age = minutes;
    const packageFiles = {};
    for (const [manager, name] of [["github-actions", ".github/workflows/flake.yml"], ["nix", "flake.nix"]]) {
      // The manager as a run has it, nix (off by Renovate's default) on as this config has it.
      const res = await extractPackageFile(manager, files[name], name, mergeChildConfig(config, { manager, ...config[manager] }));
      packageFiles[manager] = res ? [{ packageFile: name, ...res }] : [];
    }
    await fetchUpdates(config, packageFiles);
    const { branches } = await branchifyUpgrades(config, packageFiles);
    for (const b of branches.filter((b) => !b.isLockFileMaintenance).sort((x, y) => x.branchName.localeCompare(y.branchName))) {
      const updates = b.upgrades.map((u) => `${u.manager} ${u.depName} ${u.updateType} ${u.newDigest.slice(0, 7)}`).sort().join(", ");
      console.log(`${label} ${minutes} min: ${b.branchName} ${b.pendingChecks ? "pending" : "opens"}: ${b.prTitle}: ${updates}`);
    }
  }
}
