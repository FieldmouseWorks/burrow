// renovate-dashboard-no-work RENOVATE: the Dependency Dashboard Renovate's own
// code writes, through a stand-in platform, for each way a branch can come
// out of a run with no PR. A refresh that had nothing to commit, has no branch
// and no artifact error is in already, so it's on no list. One whose
// artifacts failed and left nothing to commit ends the same way bar the error,
// and stays under Other Branches to be retried; so does one whose branch is
// there without a PR, and one waiting on its PR.
// Stock Renovate 44.79.2 listed the first under Other Branches, as pending,
// for good: a lock file whose refresh keeps changing nothing (burrow's npm
// one, its only package pinned exactly) sat there from one run in its
// schedule to the next, and the steward's dashboard watch took it for stuck.
import assert from "node:assert/strict";
import fs from "node:fs";

const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const platforms = (await import(`${dist}/modules/platform/api.js`)).default;
const { setPlatformApi } = await import(`${dist}/modules/platform/index.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { ensureDependencyDashboard } = await import(`${dist}/workers/repository/dependency-dashboard.js`);

// processBranch says an update is in only when it had nothing to commit,
// no branch and no artifact error, and a run keeps that where the dashboard
// reads it.
const step = fs.readFileSync(`${dist}/workers/repository/update/branch/index.js`, "utf8");
assert.match(
  step,
  /if \(!commitSha && !branchExists\) return \{\s*branchExists,\s*result: "no-work",\s*upToDate: !config\.artifactErrors\?\.length\s*\}/,
  "a branch step with nothing to commit says whether its artifacts failed",
);
const write = fs.readFileSync(`${dist}/workers/repository/process/write.js`, "utf8");
assert.match(write, /branch\.upToDate = res\?\.upToDate;/, "a run keeps whether its update is in");

let body;
// In local's place, whose scm needs nothing.
platforms.set("local", {
  maxBodyLength: () => 60000,
  massageMarkdown: (text) => text,
  ensureIssue: async ({ body: b }) => {
    body = b;
    return "updated";
  },
  ensureIssueClosing: async () => {},
});
setPlatformApi("local");
GlobalConfig.set({});

const branch = (branchName, more) => ({
  branchName,
  prTitle: branchName,
  upgrades: [{ updateType: "lockFileMaintenance" }],
  ...more,
});
await ensureDependencyDashboard(
  {
    dependencyDashboard: true,
    dependencyDashboardTitle: "Dependency Dashboard",
    dependencyDashboardChecks: {},
  },
  [
    branch("nothing-to-commit", { result: "no-work", upToDate: true }),
    branch("artifacts-failed", { result: "no-work", upToDate: false }),
    branch("branch-no-pr", { result: "no-work" }),
    branch("awaiting-tests", { result: "done", prBlockedBy: "AwaitingTests" }),
  ],
  {},
  { result: "no-migration" },
);
const others = body.split("## Other Branches")[1]?.split("\n## ")[0] ?? "";
assert.ok(!body.includes("nothing-to-commit"), `a refresh with nothing to commit is listed:\n${body}`);
assert.ok(others.includes("other-branch=artifacts-failed"), `a refresh whose artifacts failed isn't under Other Branches:\n${body}`);
assert.ok(others.includes("other-branch=branch-no-pr"), `a branch with no PR isn't under Other Branches:\n${body}`);
assert.ok(others.includes("other-branch=awaiting-tests"), `a PR waiting on tests isn't under Other Branches:\n${body}`);
console.log("renovate-dashboard-no-work: ok");
