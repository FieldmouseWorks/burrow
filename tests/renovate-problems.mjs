// renovate-problems RENOVATE CONFIG: what a dashboard's Repository Problems
// would list under CONFIG's log level remaps, as Renovate's own logger files
// it, one line each: every warning Renovate gives for an update that went
// ahead with no release timestamp, by the words its own code uses (one for
// the upgrade, when it builds the branch, and one for the release, when it
// looks the update up), and one that's a real problem.
import fs from "node:fs";

const [renovate, file] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { init, logger, getProblems } = await import(`${dist}/logger/index.js`);
const { setGlobalLogLevelRemaps, setRepositoryLogLevelRemaps } = await import(`${dist}/logger/remap.js`);
const config = JSON.parse(fs.readFileSync(file, "utf8"));
const warnings = new Set();
for (const source of [
  "workers/repository/update/branch/index.js",
  "workers/repository/process/lookup/index.js",
  "workers/repository/process/lookup/filter-checks.js",
]) {
  const text = fs.readFileSync(`${dist}/${source}`, "utf8");
  for (const [, warning] of text.matchAll(/logger\.once\.warn\("(Some \w+\(s\) did not have a releaseTimestamp[^"]*)"\)/g)) warnings.add(warning);
}
// Both kinds, or Renovate has moved one and this check would pass on less.
for (const kind of ["upgrade", "release"]) {
  if (![...warnings].some((w) => w.startsWith(`Some ${kind}(s) `))) throw new Error(`no ${kind} warning found in Renovate's code`);
}
await init();
setGlobalLogLevelRemaps(config.logLevelRemap);
setRepositoryLogLevelRemaps(config.logLevelRemap);
for (const warning of warnings) logger.once.warn(warning);
logger.warn("artifactErrors");
for (const { msg } of getProblems()) console.log(msg);
