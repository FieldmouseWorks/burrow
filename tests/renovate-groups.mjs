// renovate-groups RENOVATE CONFIG: the branches Renovate makes under
// CONFIG, presets resolved as a run resolves them, for a repo whose npm
// and Cargo dependencies each have an update: minor ones from a monorepo,
// from packages Renovate's own presets group, and from anywhere else, and
// majors from a monorepo and from a package with no source repo. Renovate's
// own code applies the rules and branches the updates. One line per
// branch: its name, then each update in it.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [renovate, file] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { getConfig } = await import(`${dist}/config/defaults.js`);
const { mergeChildConfig } = await import(`${dist}/config/utils.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { mergeRenovateConfig } = await import(`${dist}/workers/repository/init/merge.js`);
const { branchifyUpgrades } = await import(`${dist}/workers/repository/updates/branchify.js`);

const dir = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-groups-"));
GlobalConfig.set({ requireConfig: "ignored", localDir: dir, cacheDir: path.join(dir, ".cache") });
const config = await mergeRenovateConfig(mergeChildConfig(getConfig(), JSON.parse(fs.readFileSync(file, "utf8"))));
Object.assign(config, { repository: "sam/project", baseBranch: "main", repoIsOnboarded: true, errors: [], warnings: [] });

// A dependency on NAME, from SOURCE's repo on GitHub (none: no source
// repo), at version FROM with an update to TO.
const dep = (datasource, name, source, from, to) => {
  const [major, minor] = to.split(".").map(Number);
  const updateType = major > Number(from.split(".")[0]) ? "major" : "minor";
  return {
    depName: name, packageName: name, datasource, currentValue: from, currentVersion: from,
    ...(source ? { sourceUrl: `https://github.com/${source}` } : {}),
    updates: [{ updateType, newValue: to, newVersion: to, newMajor: major, newMinor: minor, releaseTimestamp: "2026-10-01T00:00:00Z" }],
  };
};
const packageFiles = {
  npm: [{ packageFile: "package.json", deps: [
    dep("npm", "@babel/core", "babel/babel", "7.20.0", "7.21.0"),
    dep("npm", "@babel/preset-env", "babel/babel", "7.20.0", "7.21.0"),
    dep("npm", "react", "facebook/react", "19.0.0", "19.1.0"),
    dep("npm", "puppeteer", "puppeteer/puppeteer", "24.0.0", "24.1.0"),
    dep("npm", "@types/node", "DefinitelyTyped/DefinitelyTyped", "24.0.0", "24.1.0"),
    dep("npm", "jest", "jestjs/jest", "29.0.0", "30.0.0"),
    dep("npm", "@jest/globals", "jestjs/jest", "29.0.0", "30.0.0"),
    dep("npm", "left-pad", null, "1.0.0", "2.0.0"),
  ] }],
  cargo: [{ packageFile: "Cargo.toml", deps: [
    dep("crate", "serde", "serde-rs/serde", "1.0.0", "1.1.0"),
    dep("crate", "tokio", "tokio-rs/tokio", "1.0.0", "1.1.0"),
  ] }],
};
const { branches } = await branchifyUpgrades(config, packageFiles);
for (const b of branches.filter((b) => !b.isLockFileMaintenance).sort((x, y) => x.branchName.localeCompare(y.branchName))) {
  console.log(`${b.branchName}: ${b.upgrades.map((u) => `${u.depName} ${u.updateType}`).sort().join(", ")}`);
}
