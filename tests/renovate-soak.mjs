// renovate-soak RENOVATE CONFIG DEP...: the release age each DEP (JSON:
// datasource, packageName, and the registryUrl and manager it comes with)
// waits for under CONFIG's package rules, as Renovate's own code applies
// them, one line each: "DATASOURCE NAME[ at REGISTRY] AGE", "none" for none.
// A DEP's sourceUrl is what its lookup says: GitHub's datasources build it
// from the name and registry (as here); anything else gives the one it was
// handed, which a DEP sets to what a registry's package claims.
import fs from "node:fs";

const [renovate, file, ...deps] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { applyPackageRules } = await import(`${dist}/util/package-rules/index.js`);
const { getSourceUrl } = await import(`${dist}/util/github/url.js`);
const config = JSON.parse(fs.readFileSync(file, "utf8"));
for (const arg of deps) {
  const { datasource, packageName, registryUrl, manager = "github-actions", sourceUrl } = JSON.parse(arg);
  const dep = {
    ...config, manager, datasource, depName: packageName, packageName, updateType: "digest",
    ...(registryUrl ? { registryUrls: [registryUrl] } : {}),
  };
  // Renovate applies the rules once before the lookup, and again with the
  // source URL the lookup found.
  const before = await applyPackageRules(dep);
  const url = datasource.startsWith("github-") ? getSourceUrl(packageName, registryUrl) : sourceUrl;
  const got = await applyPackageRules({ ...before, sourceUrl: url }, "source-url");
  console.log(`${datasource} ${packageName}${registryUrl ? ` at ${registryUrl}` : ""} ${got.minimumReleaseAge ?? "none"}`);
}
