# The repo baseline

Every repo the autoland steward covers (`burrow.autoland.owners`) is held to
this, new ones included. The steward keeps the **Settings** itself, every
day. When it first sees a repo, a Claude job brings the **Files** up to it in
one draft PR, which then goes through the same review loop as any other.

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

## The PR flow (the steward)

1. PRs are opened as drafts.
2. Codex reviews the head (the steward asks with `@codex review`). Where
   the machine turns on `burrow.autoland.deepseek`, DeepSeek reviews it too,
   in the background: the steward sends it the diff and the Code Review Rules
   below and posts its findings as review threads marked as DeepSeek's.
3. Whoever opened the PR fixes each finding, or replies with why not, and
   resolves its thread. A Claude job on the machine takes over if nobody has
   after `handoffAfterMinutes`.
4. The reviewers look again, until none of them finds anything on the exact head.
5. With no thread open, no changes requested and CI green, the steward posts
   `review-settled` and marks the PR ready. Codex takes marking ready as a
   cue to review again, so the steward merges a pass later, once that look
   has had its chance; the branch is deleted.

Waiting for the owner instead: PRs touching the one-way paths
(`burrow.autoland.oneWay`: secrets, migrations), PRs past the
round limit, major dependency updates, outside contributions, and PRs in a
repo with no CI yet (unless it's set `requireCi = false`: nothing to test), and
settled PRs waiting only on a check that a person clears, such as a test
machine's verdict (the repo names it in `ownerChecks`). The steward marks a PR the owner merges ready, a pass before listing it, so the owner's merge doesn't set off a review of its own. They're listed in one issue. Dependency bots' minor updates skip review: CI is their
review. Changes to `.github/` aren't one-way: the reviewers read them like any
other change, and the steward merges them once they're settled.

## Files (a Claude job, once)

- **CI** on every PR that builds the code and runs its tests, in any repo with
  code. Pin actions to a commit (Renovate keeps them current). Without CI,
  "green" means nothing and dependency updates can't merge themselves.
  The CI is the flake gate the [house style](house-style.md#the-gate-is-the-flake)
  describes: a `flake.nix` from burrow's template, and the workflow that calls
  `nix flake check`. Jobs the flake can't hold stay alongside it.
- **AGENTS.md** with the two sections below, plus whatever the repo's own
  agents need (how to build, test and run it). Keep the repo's existing text.
  Codex (and DeepSeek) apply only the `## Code Review Rules` section of the AGENTS.md
  nearest each changed file (`###` subheadings group its checks), so review
  rules under any other heading move under that one.
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

Open PRs as drafts and leave them to the steward: it asks the review bot for a
review, and once the bot finds nothing on your head, every review thread is
resolved and CI is green, it marks the PR ready and merges it. Fix or answer every finding:
push the fix and reply with the commit, or reply with why it's wrong, then
resolve the thread. Don't merge or mark ready yourself.

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
