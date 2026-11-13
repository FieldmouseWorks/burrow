// The wrapper's contract with Renovate (crates/burrow-renovate, run.rs):
// discovery narrowed by autodiscoverFilter still lists the installation, and
// a rate limit met there reaches the JSON error log, through the file
// descriptor the wrapper hands it, as typed rate-limit-exceeded errors.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "renovate-errors-"));
const file = path.join(dir, "errors");
const fd = fs.openSync(file, "w+");
try {
  process.env.RENOVATE_LOG_FILE = `/dev/fd/${fd}`;
  process.env.RENOVATE_LOG_FILE_LEVEL = "error";
  process.env.RENOVATE_LOG_FILE_FORMAT = "json";
  process.env.RENOVATE_LOG_LEVEL = "fatal";
  const { init, logger } = await import(`${dist}/logger/index.js`);
  await init();
  const { autodiscoverRepositories } = await import(`${dist}/workers/global/autodiscover.js`);
  const { default: platforms } = await import(`${dist}/modules/platform/api.js`);
  const { setPlatformApi } = await import(`${dist}/modules/platform/index.js`);
  let discoveries = 0;
  platforms.set("github", {
    ...platforms.get("github"),
    getRepos: async () => { discoveries++; throw new Error("rate-limit-exceeded"); },
  });
  setPlatformApi("github");
  const err = await autodiscoverRepositories({
    platform: "github", autodiscover: true, autodiscoverFilter: ["Sam/R"],
  }).catch((e) => e);
  assert.equal(discoveries, 1);
  assert.equal(err?.message, "rate-limit-exceeded");

  // As GitHub's getRepos and the global worker log it (#780).
  logger.error({ err }, "GitHub getRepos error");
  logger.fatal({ err }, "Unknown error");
  const records = fs.readFileSync(file, "utf8").trim().split("\n").map(JSON.parse);
  assert.deepEqual(records.map((r) => [r.level, r.err.message]), [
    [50, "rate-limit-exceeded"], [60, "rate-limit-exceeded"],
  ]);
} finally {
  fs.closeSync(fd);
  fs.rmSync(dir, { recursive: true });
}
