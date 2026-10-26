# The repo baseline

Every repo the autoland steward covers (`burrow.autoland.owners`) is held to
this, new ones included. The steward keeps the **Settings** itself, every
day. When it first sees a repo, a Claude job brings the **Files** up to it in
one PR, opened ready for review, which then goes through the same review loop
as any other.

A Claude bringing a repo up to the baseline: add only what's missing, keep
what the repo already does better (a stricter gate, its own license), and
change nothing else in the same PR.

## Settings (the steward)

- Auto-merge allowed; head branches deleted on merge; squash merge allowed.
  The other merge methods stay as the repo has them.
- Public repos: secret scanning and push protection on. Dependabot alerts on
  everywhere.
- A `baseline` ruleset on the default branch, where the plan allows one: no
  force pushes or deleting the branch, changes only through PRs, review
  threads resolved, CODEOWNERS honoured, and the `review-settled` status
  required. Repo admins can still merge past it from the PR page. A repo whose
  automation pushes to its default branch (`directPush`) gets only the first
  two. Existing rules stay; the ruleset only adds.
- No rule makes a branch be up to date with its base before it merges, the
  repo's own rulesets and branch protection included: the steward turns that
  off where it finds it. With it on, every merge puts every other open PR
  behind, and each needs a new merge, CI run and review before it can land,
  so the PRs take turns. It guards only against two PRs that pass alone and
  break the branch together, and the default branch's own CI catches that
  (step 8 of the PR flow undoes it). There's no merge queue either.
- The `hold` and `needs-owner` labels (`burrow.autoland.labels`), which the
  steward and its jobs put on PRs.

## The PR flow (the steward)

1. PRs are opened ready for review. A draft is work in progress: the steward
   leaves it alone (a pending `review-settled`, nothing else) until it's ready.
2. The machine's reviewers (`burrow.autoland.reviewers`) review the head:
   Codex through the codex CLI on the owner's ChatGPT plan, and DeepSeek
   through DeepSeek Harness. The steward takes a snapshot of the change with
   secrets paths left out; each agent reads it read-only, with no GitHub
   login, and the steward posts its findings as review threads marked as
   that reviewer's. One whose usage runs out is left out until it's back,
   and PRs settle on the others meanwhile.
3. Whoever opened the PR fixes each finding, or replies with why not, and
   resolves its thread. A Claude job on the machine takes over if nobody has
   after `handoffAfterMinutes`.
4. The reviewers look again, until none of them finds anything on the exact head.
5. With no thread open, no changes requested and CI green, the steward posts
   `review-settled` and merges it, `.github/` changes included; the branch is
   deleted.
6. A settled PR it won't land on its own word gets one Claude job a head
   that judges it: one touching a one-way path (`burrow.autoland.oneWay`:
   CODEOWNERS, secrets, migrations), one in a repo with no CI yet (the job
   builds and tests it itself, unless the repo is set `requireCi = false`:
   nothing to test), a dependency update touching a one-way path, and a
   Dependabot major. The job lands anything a revert or a follow-up can undo,
   and sends the owner only what can't be undone (data dropped or rewritten
   for good, a secret exposed) or is theirs to decide. Its verdict is a
   comment ending `<!-- autoland-judgment: land|owner|failed SHA SEAL -->`,
   trusted only for that exact head, only from the steward's own login, and
   only with the seal the steward gave that job for that verdict: every
   Claude job posts as that login, so a job quoting PR text isn't a verdict.
7. Checks that run on a machine rather than in CI (the repo names them in
   `proofJobs`, with how to run each): once a settled PR waits on nothing
   else, one Claude job a head runs them on that commit and records each
   verdict. A red one goes to a fix job like any red CI.
8. A red default branch (CI failed on what was merged): one Claude job
   finds the cause and opens a PR from `autoland/red-SHA` that reverts the
   merge that broke it, or fixes it, and that PR lands like any other. Nothing
   more while the job works or its PR is open; one more job if the branch is
   still red after, then the owner. Green again without it (fixed some other
   way, or a flake), its PR is closed once the job has ended.

Waiting for the owner instead, listed in one issue: what a judge job sent
them (or anyone labelled `needs-owner`), PRs past the round limit, held
Renovate majors, and outside contributions. Everything else is
pre-approved: merges, GitHub settings changes and cleanup don't wait for
the owner. Dependency bots' minor updates skip review: CI is their review,
except a change to `.github/` beyond repinning the actions its workflows
use: that's the gate, so it goes through the review loop like any other PR.
Apart from CODEOWNERS, `.github/` isn't one-way. A Renovate
major merges once its CI is green and renovate-sweep's job has given it an
ok verdict for that head; a security fix merges on green CI alone, a major
one too. An ok major in a repo the steward doesn't cover is listed for the
owner, since nothing else merges it.

## Files (a Claude job, once)

- **CI** on every PR that builds the code and runs its tests, in any repo with
  code. Pin actions to a commit (Renovate keeps them current). Without CI,
  "green" means nothing and dependency updates can't merge themselves.
  The CI is the flake gate the [house style](house-style.md#the-gate-is-the-flake)
  describes: a `flake.nix` from burrow's template, and the workflow that calls
  `nix flake check`. Jobs the flake can't hold stay alongside it.
- **AGENTS.md** with the two sections below, plus whatever the repo's own
  agents need (how to build, test and run it). Keep the repo's existing text.
  The reviewers apply only the `## Code Review Rules` section of the
  AGENTS.md nearest each changed file, as the base branch has it (`###`
  subheadings group its checks), so review rules under any other heading
  move under that one.
- **The house block** in AGENTS.md: [`house/rules.md`](../house/rules.md),
  copied in verbatim with its marker lines, as [the house style](house-style.md)
  says, so the house sync can find and update it later.
- **CLAUDE.md** containing `@AGENTS.md`, then anything only Claude needs, so
  there's one source of truth.
- **README.md**: what it is, how to build and run it, and its license.
- **.gitignore** for the language's build output and editor files.
- **LICENSE** in public repos (ask the owner which, if there's none).
- **One dependency bot.** Renovate covers every repo from the machine (no
  per-repo config needed; a `renovate.json` only adds to it). Don't also have
  Dependabot version updates in `.github/dependabot.yml`, unless the repo is
  set aside for Dependabot on purpose.
- **CODEOWNERS** only for paths the owner must sign off on themselves, never
  `*`: the owner can't approve PRs their own Claude sessions open.

### AGENTS.md: the shared sections

They call Codex "the review bot", so they pass the name guards some repos run
over their guidance (and the house rule against naming models).

```markdown
## Pull requests

Open PRs ready for review, never as drafts, and leave them to the steward: it
asks the review bot for a review, and once the bot finds nothing on your head,
every review thread is resolved and CI is green, it merges the PR. Fix or
answer every finding: push the fix and reply with the commit, or reply with
why it's wrong, then resolve the thread. A PR touching a one-way path
(secrets, migrations, CODEOWNERS) gets a judgment first: it lands unless it
can't be undone or is the owner's to decide.

## Code Review Rules

For reviewers, the review bot included. Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
- <the repo's own risks: what a reviewer of this codebase must never let through>
```

## Hygiene (the steward, daily)

- Branches whose tip is already in the default branch, with no open PR, are
  deleted (nothing is lost). Bots' branches and `keepBranches` are left alone.
- Unmerged branches with no PR and no commit for 30 days are listed for the
  owner, never deleted.
