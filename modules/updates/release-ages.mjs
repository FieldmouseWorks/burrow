// release-ages RENOVATE < CONFIG: what a Renovate config (JSON) sets that
// decides a lock file refresh's release age, as JSON on stdout: every
// release age it sets, in milliseconds (ages), the config files it adds
// (configFileNames, left out when it doesn't set it) and whether it inherits config
// from another repo (inheritConfig).
// It asks Renovate's own code: the config is migrated (stabilityDays and
// the like) and every preset it extends, at any scope and through every
// preset those extend, resolved, as Renovate does; every age is read as
// Renovate reads it. A preset it can't resolve, an age it can't read, or
// configFileNames anywhere but the top level exits 1. statusCheckNames and
// statusCheckWhen use minimumReleaseAge for a check's name and when it's
// set, not for an age.
const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { migrateConfig } = await import(`${dist}/config/migration.js`);
const { resolveConfigPresets } = await import(`${dist}/config/presets/index.js`);
const { toMs } = await import(`${dist}/util/pretty-time.js`);
let input = "";
for await (const chunk of process.stdin) input += chunk;
const { config } = await resolveConfigPresets(migrateConfig(JSON.parse(input)).migratedConfig);
const ages = [];
let inheritConfig = false;
(function walk(node, top) {
  if (Array.isArray(node)) return node.forEach((x) => walk(x, false));
  if (node === null || typeof node !== "object") return;
  for (const [key, value] of Object.entries(node)) {
    if (key.startsWith("statusCheck")) continue;
    if (key === "inheritConfig") inheritConfig ||= Boolean(value);
    else if (key === "configFileNames" && !top) throw new Error("configFileNames is set below the top level");
    else if (key !== "minimumReleaseAge") walk(value, false);
    else if (value !== null) {
      const ms = typeof value === "string" ? toMs(value) : null;
      if (ms === null) throw new Error(`can't read the release age ${JSON.stringify(value)}`);
      ages.push(ms);
    }
  }
})(config, true);
const files = "configFileNames" in config ? { configFileNames: config.configFileNames } : {};
console.log(JSON.stringify({ ages, ...files, inheritConfig }));
