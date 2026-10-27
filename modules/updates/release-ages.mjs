// release-ages RENOVATE < CONFIG: every release age a Renovate config
// (JSON) sets, in milliseconds, as a JSON list on stdout.
// It asks Renovate's own code: every preset the config extends, at any
// scope and through every preset those extend, is resolved as Renovate
// resolves it, and every age is read as Renovate reads it. A preset it
// can't resolve, or an age it can't read, exits 1. statusCheckNames and
// statusCheckWhen use minimumReleaseAge for a check's name and when it's
// set, not for an age.
const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { resolveConfigPresets } = await import(`${dist}/config/presets/index.js`);
const { toMs } = await import(`${dist}/util/pretty-time.js`);
let input = "";
for await (const chunk of process.stdin) input += chunk;
const { config } = await resolveConfigPresets(JSON.parse(input));
const ages = [];
(function walk(node) {
  if (Array.isArray(node)) return node.forEach(walk);
  if (node === null || typeof node !== "object") return;
  for (const [key, value] of Object.entries(node)) {
    if (key.startsWith("statusCheck")) continue;
    if (key !== "minimumReleaseAge") walk(value);
    else if (value !== null) {
      const ms = typeof value === "string" ? toMs(value) : null;
      if (ms === null) throw new Error(`can't read the release age ${JSON.stringify(value)}`);
      ages.push(ms);
    }
  }
})(config);
console.log(JSON.stringify(ages));
