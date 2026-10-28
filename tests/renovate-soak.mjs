// renovate-soak RENOVATE CONFIG NAME...: the release age each NAME (a pinned
// action or workflow's OWNER/REPO) waits for under CONFIG's package rules, as
// Renovate's own code applies them, one line each: "NAME AGE", "none" for
// none.
import fs from "node:fs";

const [renovate, file, ...names] = process.argv.slice(2);
const { applyPackageRules } = await import(`${renovate}/lib/node_modules/renovate/dist/util/package-rules/index.js`);
const config = JSON.parse(fs.readFileSync(file, "utf8"));
for (const packageName of names) {
  const dep = { ...config, manager: "github-actions", datasource: "github-digest", depName: packageName, packageName, updateType: "digest" };
  const got = await applyPackageRules(dep);
  console.log(`${packageName} ${got.minimumReleaseAge ?? "none"}`);
}
