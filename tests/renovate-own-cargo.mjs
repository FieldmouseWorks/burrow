// renovate-own-cargo RENOVATE CONFIG MANIFEST: which of MANIFEST's cargo
// dependencies Renovate moves under CONFIG (this repo's own renovate.json),
// as Renovate's own cargo manager extracts them and its own package rules
// apply, one line each, "DEPNAME PACKAGENAME on|off", sorted. A git URL of
// the same host that isn't Conary's comes last, as a stand-in a rule
// written too wide would also turn off.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [renovate, file, manifest] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { extractPackageFile } = await import(`${dist}/modules/manager/cargo/extract.js`);
const { applyPackageRules } = await import(`${dist}/util/package-rules/index.js`);

// The extraction looks for the repo's .cargo/config: an empty checkout has none.
GlobalConfig.set({ localDir: fs.mkdtempSync(path.join(os.tmpdir(), "renovate-own-")) });
const config = JSON.parse(fs.readFileSync(file, "utf8"));
const packageFile = "crates/Cargo.toml";
const extracted = await extractPackageFile(fs.readFileSync(manifest, "utf8"), packageFile, {});
const deps = [
  ...extracted.deps.filter((d) => d.datasource !== "crate" || d.depName === "serde"),
  { depName: "other", packageName: "https://github.com/FieldmouseWorks/Conary-other", datasource: "git-refs" },
];
const lines = [];
for (const dep of deps) {
  const got = await applyPackageRules({ ...config, manager: "cargo", packageFile, ...dep });
  lines.push(`${dep.depName} ${dep.packageName ?? dep.depName} ${got.enabled === false ? "off" : "on"}`);
}
console.log([...lines.slice(0, -1).sort(), lines.at(-1)].join("\n"));
