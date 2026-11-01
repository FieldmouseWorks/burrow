// renovate-cargo-unchanged RENOVATE: what Renovate's own cargo manager makes
// of a lock file refresh, with a stand-in cargo first on PATH: a lock file
// `cargo update` leaves alone is no change, and one it changes is the new one.
// Stock Renovate 44.79.2 compared two Buffers with ===, so it took every
// refresh for a change and committed nothing new: "Detected empty commit" on
// the repo's dashboard, every run, for as long as its crates stayed current.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { updateArtifacts } = await import(`${dist}/modules/manager/cargo/artifacts.js`);

const root = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-cargo-"));
const [localDir, cacheDir, bin] = ["repo", "cache", "bin"].map((d) => path.join(root, d));
for (const d of [path.join(localDir, "member"), cacheDir, bin]) fs.mkdirSync(d, { recursive: true });
// Adds to the lock file what the case says: exec gives cargo no env of ours.
const adds = path.join(root, "adds");
fs.writeFileSync(path.join(bin, "cargo"), `#!/bin/sh\ncat "${adds}" >> Cargo.lock\n`, { mode: 0o755 });
process.env.PATH = `${bin}:${process.env.PATH}`;
GlobalConfig.set({ localDir, cacheDir, binarySource: "global" });

const lock = 'version = 4\n';
const refresh = async (member, added) => {
  fs.writeFileSync(adds, added);
  return updateArtifacts({
    packageFileName: member,
    updatedDeps: [],
    newPackageFileContent: "[package]\n",
    config: { isLockFileMaintenance: true },
  });
};

fs.writeFileSync(path.join(localDir, "Cargo.lock"), lock);
assert.equal(await refresh("Cargo.toml", ""), null, "a refresh that changes nothing is no change");

const [{ file }] = await refresh("Cargo.toml", 'name = "new"\n');
assert.equal(file.path, "Cargo.lock");
assert.equal(file.contents.toString(), `${lock}name = "new"\n`);

// The next member of the workspace finds the refresh already done.
assert.equal(await refresh("member/Cargo.toml", ""), null, "a workspace's next member adds nothing");
console.log("renovate-cargo-unchanged: ok");
