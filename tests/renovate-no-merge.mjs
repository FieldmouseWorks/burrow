// renovate-no-merge RENOVATE: Renovate's own merges, through a stand-in
// platform and scm that count them, with an update its config has merge on
// green, by a PR, a comment or a branch pushed onto its base, and the
// platform's own automerge on. With BURROW_RENOVATE_NO_MERGE in its
// environment (renovate-run, in a repo the steward doesn't land) it merges
// none of them, and the branch gets a PR instead; without, each merges.
// Stock Renovate merged them all whatever the environment, and a security
// fix's own `force` turns automerge back on over a run's forced `false`.
// Also renovate-run's words as Renovate's own code reads them: its forced
// `--automerge=false`, and a full run's filter, whose `!` leaves a repo out
// however GitHub spells it.
import assert from "node:assert/strict";

const [renovate] = process.argv.slice(2);
const dist = `${renovate}/lib/node_modules/renovate/dist`;
const platforms = (await import(`${dist}/modules/platform/api.js`)).default;
const { platformScmImpls } = await import(`${dist}/modules/platform/scm.js`);
const { setPlatformApi } = await import(`${dist}/modules/platform/index.js`);
const { GlobalConfig } = await import(`${dist}/config/global.js`);
const { checkAutoMerge } = await import(`${dist}/workers/repository/update/pr/automerge.js`);
const { tryBranchAutomerge } = await import(`${dist}/workers/repository/update/branch/automerge.js`);
const { getPlatformPrOptions } = await import(`${dist}/workers/repository/update/pr/index.js`);
const { getConfig } = await import(`${dist}/workers/global/config/parse/cli.js`);
const { applyFilters } = await import(`${dist}/workers/global/autodiscover.js`);

// Every way it could move the base branch, counted.
let moved = [];
platforms.set("github", {
  ...platforms.get("github"),
  getBranchStatus: async () => "green",
  getBranchPr: async () => null,
  isBranchMergeQueueEnabled: async () => false,
  mergePr: async ({ id }) => (moved.push(`merge #${id}`), true),
  ensureComment: async ({ number, content }) => (moved.push(`comment #${number} ${content}`), true),
  ensureCommentRemoval: async () => {},
});
platformScmImpls.set("github", class {
  isBranchConflicted = async () => false;
  isBranchModified = async () => false;
  checkoutBranch = async () => "sha";
  mergeAndPush = async (branch) => { moved.push(`push ${branch}`); };
  deleteBranch = async () => {};
});
setPlatformApi("github");
GlobalConfig.set({});

const update = (automergeType) => ({
  branchName: `renovate/${automergeType}`, baseBranch: "main", automerge: true, automergeType,
  automergeStrategy: "squash", automergeComment: "automergeComment", platformAutomerge: true,
  automergeSchedule: ["at any time"], schedule: ["at any time"],
});
// A PR of its own each pass: Renovate remembers the comments it made.
async function every(number) {
  const pr = { number, title: "deps: update x", sourceBranch: "renovate/pr" };
  moved = [];
  const results = [
    (await checkAutoMerge(pr, update("pr"))).automerged,
    (await checkAutoMerge(pr, update("pr-comment"))).automerged,
    await tryBranchAutomerge(update("branch")),
    getPlatformPrOptions(update("pr")).usePlatformAutomerge,
    getPlatformPrOptions(update("branch")).usePlatformAutomerge,
  ];
  return { results, moved };
}

delete process.env.BURROW_RENOVATE_NO_MERGE;
assert.deepEqual(await every(7), {
  results: [true, true, "automerged", true, true],
  moved: ["merge #7", "comment #7 automergeComment", "push renovate/branch"],
}, "free, each way merges");

process.env.BURROW_RENOVATE_NO_MERGE = "1";
assert.deepEqual(await every(8), {
  results: [false, false, "no automerge", false, false],
  moved: [],
}, "held, none merges, and a branch gets a PR");
delete process.env.BURROW_RENOVATE_NO_MERGE;

// renovate-run's words, as Renovate's command line takes them.
assert.equal(getConfig(["node", "renovate", "--automerge=false"]).automerge, false);
assert.deepEqual(
  applyFilters(["Sam/R", "sam/a", "Kim/Q", "kim/p"], ["sam/*", "kim/*", "!kim/q", "!sam/r"]),
  ["sam/a", "kim/p"],
);
