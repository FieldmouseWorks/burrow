// renovate-problems RENOVATE CONFIG: what a dashboard's Repository Problems
// would list under CONFIG's log level remaps, as Renovate's own logger files
// it, one line each: the warning Renovate gives for a refresh with no release
// timestamp, by the words its own code uses, and one that's a real problem.
import fs from "node:fs";

const [renovate, file] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const { init, logger, getProblems } = await import(`${dist}/logger/index.js`);
const { setGlobalLogLevelRemaps, setRepositoryLogLevelRemaps } = await import(`${dist}/logger/remap.js`);
const config = JSON.parse(fs.readFileSync(file, "utf8"));
const source = fs.readFileSync(`${dist}/workers/repository/update/branch/index.js`, "utf8");
const [, warning] = source.match(/logger\.once\.warn\("(Some upgrade\(s\) did not have a releaseTimestamp[^"]*)"\)/);
await init();
setGlobalLogLevelRemaps(config.logLevelRemap);
setRepositoryLogLevelRemaps(config.logLevelRemap);
logger.once.warn(warning);
logger.warn("artifactErrors");
for (const { msg } of getProblems()) console.log(msg);
