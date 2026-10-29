// renovate-automerge RENOVATE CONFIG: whether CONFIG's package rules have
// Renovate merge each kind of update itself, as Renovate's own code applies
// them, one line each: "UPDATETYPE yes" or "UPDATETYPE no".
import fs from "node:fs";

const [renovate, file] = process.argv.slice(2);
const { applyPackageRules } = await import(`${renovate}/lib/node_modules/renovate/dist/util/package-rules/index.js`);
const config = JSON.parse(fs.readFileSync(file, "utf8"));
for (const updateType of ["major", "minor", "patch", "pin", "pinDigest", "digest", "lockFileMaintenance"]) {
  const got = await applyPackageRules({
    ...config, manager: "github-actions", datasource: "github-digest",
    depName: "actions/checkout", packageName: "actions/checkout", updateType,
  });
  console.log(`${updateType} ${got.automerge ? "yes" : "no"}`);
}
