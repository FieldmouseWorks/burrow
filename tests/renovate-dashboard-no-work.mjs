// renovate-dashboard-no-work RENOVATE: the Dependency Dashboard Renovate's own
// code writes, through a stand-in platform, for each way a branch can come
// out of a run with no PR. A refresh that had nothing to commit and has no
// branch is in already, so it's on no list; one whose branch is there without
// a PR is still under Other Branches, and so is one waiting on its PR.
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

// A run records each branch's own result, where the dashboard reads it.
const write = fs.readFileSync(`${dist}/workers/repository/process/write.js`, "utf8");
assert.match(write, /branch\.branchExists = res\?\.branchExists;/, "a run keeps whether its branch exists");

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
    branch("nothing-to-commit", { result: "no-work", branchExists: false }),
    branch("branch-no-pr", { result: "no-work", branchExists: true }),
    branch("awaiting-tests", { result: "done", branchExists: true, prBlockedBy: "AwaitingTests" }),
  ],
  {},
  { result: "no-migration" },
);
const others = body.split("## Other Branches")[1]?.split("\n## ")[0] ?? "";
assert.ok(!body.includes("nothing-to-commit"), `a refresh with nothing to commit is listed:\n${body}`);
assert.ok(others.includes("other-branch=branch-no-pr"), `a branch with no PR isn't under Other Branches:\n${body}`);
assert.ok(others.includes("other-branch=awaiting-tests"), `a PR waiting on tests isn't under Other Branches:\n${body}`);
console.log("renovate-dashboard-no-work: ok");
