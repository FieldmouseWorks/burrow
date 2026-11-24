// renovate-cargo-build-metadata RENOVATE: one bump of a crate two members of a
// workspace use, as Renovate's own cargo manager makes it, with a stand-in
// cargo first on PATH. Renovate runs `cargo update --package NAME@OLD
// --precise NEW` once per member against the one lock file; the first moves
// it, the second finds OLD gone and fails. Renovate catches that and drops a
// dependency the lock file already has at newVersion, but compared the
// strings: toml's lock version `1.2.2+spec-1.1.0` isn't newVersion `1.2.2`,
// so stock Renovate 44.79.2 reported an artifact error on a lock file that
// was right (FieldmouseWorks/burrow#1512).
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
// What cargo says when the lock file no longer has the version asked for.
fs.writeFileSync(
  path.join(bin, "cargo"),
  `#!/bin/sh
case "$*" in
  *--precise*) echo "error: package ID specification \\\`toml@1.2.1+spec-1.1.0\\\` did not match any packages" >&2; exit 101 ;;
esac
`,
  { mode: 0o755 },
);
process.env.PATH = `${bin}:${process.env.PATH}`;
GlobalConfig.set({ localDir, cacheDir, binarySource: "global" });

// The lock file as the workspace's first member left it.
const lock = 'version = 4\n\n[[package]]\nname = "toml"\nversion = "1.2.2+spec-1.1.0"\n';
fs.writeFileSync(path.join(localDir, "Cargo.lock"), lock);

const res = await updateArtifacts({
  packageFileName: "member/Cargo.toml",
  updatedDeps: [
    {
      depName: "toml",
      packageName: "toml",
      datasource: "crate",
      currentValue: "1.0",
      newValue: "1.0",
      lockedVersion: "1.2.1+spec-1.1.0",
      newVersion: "1.2.2",
    },
  ],
  newPackageFileContent: '[package]\nname = "member"\n',
  config: {},
});
assert.ok(res, "the lock file comes back");
assert.deepEqual(res.map((r) => r.artifactError).filter(Boolean), [], "no artifact error");
assert.equal(res[0].file.contents.toString(), lock, "the lock file as the first member left it");
console.log("renovate-cargo-build-metadata: ok");
