# The repo baseline

Every repo the autoland steward covers (`burrow.autoland.owners`) is held to
this, new ones included. The steward keeps the **Settings** itself, every
day. When it first sees a repo, a Claude job brings the **Files** up to it in
one PR, opened ready for review, which then goes through the same review loop
as any other.

It first sees a repo the moment it's made: GitHub tells the steward's App
(its webhook, below), and the pass over that repo sets its settings and its
own hook and starts the baseline job, which clones it on the machine. One
made empty gets its settings and hook then, but not the ruleset, which
would turn away the push that makes its default branch; that push's pass
adds the ruleset and starts the job. Nothing about a new repo waits for
the owner. A private repo's
CI still needs a runner on the machine (burrow#236).

A repo made under the name of one that was deleted is a new repo too, though
GitHub gives it the same name, in any case, and so is one renamed or moved
into a name another repo had: the steward knows it by GitHub's databaseId
for it, the number GitHub keeps for a repo whatever it's called, which the
steward keeps with the repo's settings (for settings kept before it kept
them, by being made after the steward last settled a repo of that name, or
in the same second), on the first pass that reads it, kicked or full. Before
anything acts on it, every clone of the name in `projectsDir` is moved aside
under a hidden name, its own included: which repo a clone is of can't be
told. Then proj-inbox forgets the repo's issues (`proj-inbox forget`), once
a pass of its under way has ended, so the new one's issue N is a new job and
not the old N's, and every Claude job of the repo's is stopped, known by the
repo its kick names or by its folder. Once no review of the old one is under
way, what the steward kept about it (its settings, hook, PRs, reviews and
the reviewers' sessions) is forgotten, so the new one gets its own clone and
its own baseline job. A pass that fails partway, or finds a review still
under way, does it all again on the next.

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
  required from the steward's App (in a repo with its merge train on, the
  train's `conary/train` too, from the same App). Repo admins can still
  merge past it from the PR page, except with the train on: there no one
  bypasses it, so only the train lands. A repo whose
  automation pushes to its default branch (`directPush`) gets only the first
  two. A `baseline` that lacks one of these gets it the next time the settings
  are kept, whenever it was made. Existing rules stay; the ruleset only adds. An org's ruleset doesn't
  stand in for it, whatever its name: the steward can't change the org's, so
  the repo gets its own as well, and GitHub enforces both.
- No rule makes a branch be up to date with its base before it merges, the
  repo's own rulesets and branch protection included: the steward turns that
  off where it finds it. With it on, every merge puts every other open PR
  behind, and each needs a new merge, CI run and review before it can land,
  so the PRs take turns. It guards only against two PRs that pass alone and
  break the branch together, and the default branch's own CI catches that
  (step 8 of the PR flow undoes it). There's no merge queue either.
- Wherever a rule requires `review-settled` (those rulesets and every
  protected branch's protection, since a PR can merge into any of them),
  it's required from the steward's App: GitHub then takes that status from
  no one else, auto-merge included. The steward pins it where it finds it,
  and straight away when the App changes.
- The steward merges a settled PR itself, at the head it checked, or not
  at all. It never leaves one for GitHub to merge later (auto-merge, a
  merge queue), which would merge whatever head the base's rules let
  through. It takes off any merge waiting on a PR, its auto-merge and its
  place in a merge queue, whenever the PR isn't settled and before it
  merges it. A settled PR behind a base that wants it up to date (an org's
  rule the steward can't turn off) is brought up to date instead, and the
  new head is settled like any other.
- The `hold`, `needs-owner` and `first` labels (`burrow.autoland.labels`),
  which the steward, its jobs and the owner put on PRs, and the steward's
  own `waiting: release age`, `waiting: proof slot` and `waiting: reviewer`,
  which say at a glance what a PR waits on while it does (its
  `review-settled` status says the rest).

## The steward's GitHub App

The steward does everything on GitHub as a GitHub App of the owner's
(`burrow.autoland.app`), never as the machine user's own login, which its
Claude jobs and every Claude session share. So only the App can post
`review-settled` or the reviewers' reviews: a status or review with the same
marks from anyone else, the owner's own login included, isn't the steward's
word, and the ledger (`burrow.ledger.reviewers`) doesn't count a merge on it
as a landing.

Make the App under the account that owns most of the repos, public only so
the owner's other accounts can install it too (installing it gives nobody
else anything). Turn its webhook on and subscribe it to the Repository
event: that's how a repo is heard the moment it's made, before it has a
hook of its own. The steward sets the webhook's URL and secret itself, to
burrow-events' (leave them to it). Give it these
repository permissions: read and write on administration (the settings and
ruleset above), contents (merges and deleting branches), issues (the labels
and the digest's issues), pull requests (reviews, comments and merges),
commit statuses, webhooks, workflows (a merge that changes
`.github/workflows`), and merge queues (taking a PR out of one someone else
put it in); read on checks and actions. Install it on each of
`owners`, for all repositories, so new ones are covered too: the repos the
steward keeps are the ones its installations reach (GitHub never reports an
App as a repo's admin). Its private key goes in `app.keyFile`, readable by
root only.

## The PR flow (the steward)

1. PRs are opened ready for review. A draft is work in progress: the steward
   leaves it alone (a pending `review-settled`, nothing else) until it's ready.
2. The machine's reviewer (`burrow.autoland.reviewers`) reviews the head:
   Codex, through the codex CLI on the owner's ChatGPT plan. The steward
   takes a snapshot of the change with secrets paths left out; the agent
   reads it read-only, with no GitHub login, and the steward posts its
   findings, as its App, as review threads marked as Codex's. A look at a head the PR has since moved past still
   finishes, and its findings are posted on the commit it read, so they go
   into the next push. Codex keeps one session per PR: every look
   resumes the one before, told which head it last read, and still judges
   the whole change at the new head. A PR's sessions are deleted for good
   once it merges or closes. While its usage is spent, or it's logged out,
   PRs wait for it, and it comes back on its own.
3. Whoever opened the PR fixes each finding, or replies with why not, and
   resolves its thread. A Claude job on the machine takes over if nobody has
   after `handoffAfterMinutes`, or after `sessionHandoffAfterMinutes` on a
   PR whose body links the Claude session that opened it, since that session
   is fixing them itself.
4. The reviewers look again, until none of them finds anything on the exact head.
5. With no thread open, no changes requested and CI green, the steward's App
   posts `review-settled` and merges it, `.github/` changes included; the
   branch is deleted. In a repo with its merge train on (`train`), it goes
   into the train instead, CI on the PR itself optional: Conary's `cn train`
   merges the PRs in the train onto the default branch together, has CI run
   on that, and once it's green posts `conary/train` on each PR's head and
   merges them in order. A red one is split until the PR that broke it is
   alone; that PR gets a `conary/train` failure, which a Claude job fixes as
   red CI. A PR that stops being settled comes out of the train.
6. A settled PR it won't land on its own word gets one Claude job a head
   that judges it: one touching a one-way path (`burrow.autoland.oneWay`:
   CODEOWNERS, secrets), one in a repo with no CI yet (the job
   builds and tests it itself, unless the repo is set `requireCi = false`:
   nothing to test), a dependency update touching a one-way path, and a
   Dependabot major. The job lands anything a revert or a follow-up can undo,
   and sends the owner only what can't be undone (data dropped or rewritten
   for good, a secret exposed) or is theirs to decide. Its verdict is a
   comment ending `<!-- autoland-judgment: land|owner|failed SHA SEAL -->`,
   trusted only for that exact head, only from one of the authors' logins
   (Claude jobs post as the machine user's), and only with the seal the
   steward gave that job for that verdict: every Claude job posts as that
   login, so a job quoting PR text isn't a verdict.
7. Checks that run on a machine rather than in CI (the repo names them in
   `proofJobs`, with how to run each): once a settled PR waits on nothing
   else, one Claude job a head runs them on that commit and records each
   verdict. A red one goes to a fix job like any red CI.
8. A red default branch (CI failed on what was merged): one Claude job
   finds the cause and opens a PR from `autoland/red-SHA` that reverts the
   merge that broke it, or fixes it, and that PR lands like any other. Nothing
   more while the job works or its PR is open; one more job if the branch is
   still red after, then the pipeline's stuck issue. Green again without it (fixed some other
   way, or a flake), its PR is closed once the job has ended.

Waiting for the owner instead, listed in one issue: what a judge job sent
them (or anyone labelled `needs-owner`), held Renovate majors, and outside
contributions. What the pipeline itself is stuck on (a reviewer that keeps
failing, a merge GitHub won't take, two Claude jobs that ended without an
answer) isn't the owner's either: it's listed in an issue of its own, which
proj's inbox hands to a Claude job. That needs the inbox on, `notify.repo`
cloned in `projectsDir` and the steward's App one of the inbox's authors
(autoland makes it one); without them it's the owner's, with why. A PR past the round limit isn't
the owner's: its reviewers stop looking, a Claude job fixes or answers their
last findings, and a judge decides. Everything else is pre-approved: merges, GitHub settings changes and
cleanup don't wait for the owner. Dependency bots' minor updates skip review: CI is their review,
except one with a commit no bot wrote (a Claude job's fix), which is code
nobody has read, or a change to `.github/` beyond repinning the actions its
workflows use, which is the gate. Either goes through the review loop like
any other PR.
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
  describes: a `flake.nix` from burrow's template, the workflow that calls
  `nix flake check`, and the template's rust-first workflow beside it. Jobs
  the flake can't hold stay alongside it.
- **AGENTS.md** with the Code Review Rules section below, plus whatever the
  repo's own agents need (how to build, test and run it). Keep the repo's
  existing text.
  The reviewers apply only the `## Code Review Rules` section of the
  AGENTS.md nearest each changed file, as the base branch has it (`###`
  subheadings group its checks), so review rules under any other heading
  move under that one.
- **The house blocks** in AGENTS.md: [`house/rules.md`](../house/rules.md)
  and [`house/prs.md`](../house/prs.md) (the Pull requests section), each
  copied in verbatim with its marker lines, as
  [the house style](house-style.md#the-house-blocks) says, so the house sync
  can find and update them later.
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

### AGENTS.md: the Code Review Rules section

It calls Codex "the review bot", as the Pull requests block does, so it passes
the name guards some repos run over their guidance (and the house rule against
naming models).

```markdown
## Code Review Rules

For reviewers, the review bot included. Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
- <the repo's own risks: what a reviewer of this codebase must never let through>
```

## Hygiene (the steward)

- Branches whose tip is already in the default branch, or is the head a
  closed PR was closed at, with no open PR, are deleted once they've sat an
  hour at that tip (nothing is lost: the closed PR keeps its commits and can
  restore the branch). GitHub's delete-on-merge only covers merged PRs.
  `keepBranches` are left alone, and bots' branches go only this way.
- What starts it is the branch changing: a push to it, a PR of its
  closing, the default branch moving, or the wait it was given ending. No
  daily sweep: each pass lists the branches and the PRs closed since the
  last look with the last listings' ETags, which GitHub answers for free
  while nothing changed. A branch GitHub wouldn't read for it is looked at
  again on the hourly pass.
- Unmerged branches with no PR and no commit for 3 days are archived, then
  deleted: the steward keeps the tip at `refs/archive/BRANCH/SHA` in the same
  repo, a ref GitHub keeps but lists with neither branches nor tags, and
  deletes the branch only if it's still at that tip. `git ls-remote origin
  'refs/archive/*'` lists them; `gh api repos/OWNER/REPO/git/refs -f
  ref=refs/heads/BRANCH -f sha=SHA` restores one. One GitHub won't archive
  stays, and goes to the issue of what the pipeline is stuck on.
