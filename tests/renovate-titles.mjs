// renovate-titles RENOVATE CONFIG: the PR title Renovate gives each kind of
// update under CONFIG, as Renovate's own code writes it (presets resolved,
// then the updates flattened and their branches generated, as a run does
// once it has looked them up), one line each. The title is the subject the
// steward squashes onto main.
import fs from "node:fs";

const [renovate, file] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { getConfig } = await import(`${dist}/config/defaults.js`);
const { mergeChildConfig } = await import(`${dist}/config/utils.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { mergeRenovateConfig } = await import(`${dist}/workers/repository/init/merge.js`);
const { flattenUpdates } = await import(`${dist}/workers/repository/updates/flatten.js`);
const { generateBranchConfig } = await import(`${dist}/workers/repository/updates/generate.js`);
// As a run does: Renovate's own merge puts the presets' rules after the
// config's, which is what decides a type. requireConfig "ignored" stands in for
// a repo with no config file of its own, without a platform to ask.
GlobalConfig.set({ requireConfig: "ignored" });
const config = await mergeRenovateConfig(
  mergeChildConfig(getConfig(), { ...JSON.parse(fs.readFileSync(file, "utf8")), repository: "owner/repo" }),
);

// What a lookup leaves on a dependency: its package name, and its updates.
const dep = (depName, depType, updates, more = {}) => ({ depName, packageName: depName, depType, ...more, updates });
const to = (updateType, from, version, more = {}) => ({
  updateType, currentValue: from, currentVersion: from, newValue: version, newVersion: version, ...more,
});
// A runtime dependency, which config:recommended types `fix`; a dev
// dependency, which it types `chore`; a major; a Maven dependency (`fix` by
// another of its rules); a Rust crate; an action pinned to burrow's main;
// and each manager's lock file refresh.
const updates = await flattenUpdates(config, {
  npm: [{ packageFile: "package.json", deps: [
    dep("left-pad", "dependencies", [to("patch", "1.3.0", "1.3.1")], { datasource: "npm", currentValue: "1.3.0" }),
    dep("vitest", "devDependencies", [to("minor", "3.1.0", "3.2.0")], { datasource: "npm" }),
    dep("vite", "devDependencies", [to("major", "6.0.0", "7.0.0", { newMajor: 7 })], { datasource: "npm" }),
  ] }],
  gradle: [{ packageFile: "build.gradle", deps: [
    dep("net.fabricmc:fabric-loader", "dependencies", [to("patch", "0.20.0", "0.20.1")], { datasource: "maven" }),
  ] }],
  cargo: [{ packageFile: "Cargo.toml", deps: [
    dep("tokio", "dependencies", [to("minor", "1.40.0", "1.41.0")], { datasource: "crate" }),
  ] }],
  "github-actions": [{ packageFile: ".github/workflows/flake.yml", deps: [
    dep("FieldmouseWorks/burrow", "action", [{ updateType: "digest", newValue: "main", newDigest: "0fce5b8f", newDigestShort: "0fce5b8" }],
      { datasource: "github-digest", currentValue: "main", currentDigest: "16b63d9a" }),
  ] }],
  nix: [{ packageFile: "flake.nix", deps: [] }],
});
for (const update of updates) {
  console.log(generateBranchConfig([update]).prTitle);
}
