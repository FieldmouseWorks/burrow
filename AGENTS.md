# burrow

NixOS modules for a machine you run by talking to Claude. Where things live:

- [README](README.md): each module, grouped by job, with its options.
- [docs/front-door.md](docs/front-door.md): the doors into a machine (probe,
  undo, inbox jobs, Remote Control) and how a chat thread uses them.
- [docs/repo-baseline.md](docs/repo-baseline.md): the steward, the PR flow and
  what every repo is held to.
- [docs/house-style.md](docs/house-style.md): what every repo shares (the
  house blocks in [house/](house/), the flake gate, AGENTS.md).
- [docs/flaky-tests.md](docs/flaky-tests.md): how a flaky test is retried,
  fixed and held.
- [docs/compatibility.md](docs/compatibility.md): persisted formats, their
  inventory and the `burrow-compat` harness that checks old and new readers.
- [docs/settings.md](docs/settings.md): the pipeline's options, their one
  schema (crates/burrow-settings) and `proj settings show`.

<!-- house:rules v1 begin. Copied from FieldmouseWorks/burrow house/rules.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## House rules

These hold in every repo. This file's own sections add to them and win where
they're more specific.

- Design the class, not the instance. When a bug, finding or stall shares a
  cause with others, fix the cause once, where it lives (one owning path, a
  type, a check), so none of its cases come back.
- Do work once. Look for what's already open or built first, extend the
  shared crate, client or job that does it rather than write another copy,
  and never run work twice that once would do.
- Fix the whole cause: no workarounds, shims, fallbacks or half-migrations.
  When you replace something, delete the old path in the same change.
- One fact, one home. Each rule or fact lives in one place; link to it
  instead of restating it.
- Before you edit, say what done looks like and how it will be checked.
- Report only the checks you ran and what they said. Skipped, unavailable
  or "should pass" is never a pass. Keep local and CI results apart.
- A new check comes with proof that it fails on the bug it's for.
- The code is the memory. When something goes wrong, make the code, a type,
  a test or a check stop it happening again; a new rule here is the last
  resort. Say in the PR which one you did.
- Keep going on work you were asked to do: don't ask again for what's
  already been asked for.
- Never commit what the repo keeps private: secrets, tokens, personal data,
  and anything under this file's "Private" section.
- `Closes #N` only when the issue is fully done; otherwise `Refs #N`.

CI's house check holds every PR to its title's style and to the rules below,
each of which a PR can pass with a line in its body starting as shown and a
reason of four words or more. Editing the body or title runs it again.

- Rust by default, shell included: new code meant to last starts in Rust
  unless its platform picks the language, and a coin toss goes to Rust. A PR
  that adds Python, or grows Python and shell past a fix, says `Why not
  Rust:` (a file a tool generates, glue that only runs another program, a
  throwaway script, a tool still being tuned). Port existing Python and
  shell whenever Rust buys anything, system and pipeline code first.
- Work starts from an event (a push, a webhook, a unit finishing), not a
  clock. A PR that adds a timer or a cron schedule says `Why a timer:`.
- Split a code file before it passes 1,000 lines, or say `Why not split:`.
- AGENTS.md is a map: what the repo is, its own rules, and where things live
  (a feature map, the doc that owns each subject). Detail goes in the doc
  that owns it, linked from here. A PR that grows the repo's own part (the
  lines outside these blocks) past 100 lines says `Why a longer guide:`.
- Tracked files never name a model: which model does what is the machine's
  business, and names go stale in weeks. A line that has to (a recorded
  screen in a test) says `Why a model name:`.

### Commits

Subjects are `area: plain sentence`, the area being the module, directory or
tool the change is about (`autoland: merge only the head that was checked`):
lowercase after the colon, no trailing period, no `feat:`/`fix:`. The body,
if any, says why. A PR's title is the subject main gets, so it's written the
same way. Keep the git identity your machine gives you: a commit signed with
a key that isn't its author's shows as Unverified, and CI fails it.
<!-- house:rules end -->

## Build and test

`flake-gate` is the gate, the same here as in CI
(`.github/workflows/flake.yml` says how CI splits it across runners). Don't
run a bare `nix flake check`: evaluating the whole flake at once holds 6-7G,
more than a Claude job on the machine may, so the gate evaluates each output
apart. It builds the example machine (`examples/`), evaluates each module
alone, runs `lib/project.nix`'s gates on the projects in `tests/house/`, and
builds each check, one file per check in `checks/NAME.nix`.
`nix build .#checks.x86_64-linux.agents -L` boots the agent sandboxes and
needs `/dev/kvm`.

The programs the modules run are Rust, one Cargo workspace in `crates/`: each
crate is a package of its own (`packages.SYSTEM.CRATE`) and a check
(`checks.crates-*`), and `cargo test` there runs the same tests.
`crates/burrow-workflows` tests the shared workflows' own steps and needs git,
jq, nix and rustc on PATH. [The Rust runner](crates/burrow-flake-check/README.md)
owns the shared gate and negative run.

burrow is generic: anything about one machine (its hostname, users, owner,
repos, addresses) is an option a machine's flake sets, never a value in a
module.

<!-- house:prs v1 begin. Copied from FieldmouseWorks/burrow house/prs.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## Pull requests

One task, one PR: finish what the task set out to do, then open one PR, so
it goes through review and CI once. No PR per step or round, and no stacks.
A fix to the pipeline (the steward, CI, the machine) lands as soon as it's
ready, and so does a change to these rules that the work needs, in a PR of
its own: reviewers read the default branch's rules, not the PR's.

Open PRs ready for review, never as drafts, and leave them to the steward:
once the review bot finds nothing on your head, every thread is resolved and
CI is green, it merges. Don't merge it yourself. Answer every finding: push
the fix and reply with the commit, or reply with why it's wrong, then
resolve the thread. A PR touching a one-way path (secrets, CODEOWNERS) gets
a judgment first, and lands unless it can't be undone or is the owner's
call.

A red default branch is the steward's to fix, from a branch named
`autoland/red-` and the commit. Don't fix it yourself, in any repo, unless
the pipeline's stuck issue says the steward's jobs gave up; then fix it from
a branch of that name.
<!-- house:prs end -->

## Code Review Rules

For every reviewer, the steward's included. Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
- a one-way door that stops asking: an R2 `ask` rule in `modules/claude.nix`
  dropped or narrowed, or the guard (`crates/burrow-guard`) letting a
  delete through without its snapshot or a default-branch force-push without
  asking
- a system change that can stay live without being confirmed: the switch's
  revert, or the trial boot's fallback to the last good kernel, broken or
  skippable
- a sandbox that reaches more than it's given: egress past the allowlist
  proxy, a GitHub token, or host paths beyond its clones
- the steward merging what it shouldn't: a head whose own change its
  reviewers didn't check, or a head CI didn't check, a one-way path, or a
  PR with open threads
- a review agent that reaches more than its snapshot: a GitHub token, a
  secrets path (`reviewers.skipPaths`), the PR's own copy of its review
  rules, or its own key in what gets posted
- `HOST-update` going live or confirming without the VM tests and health
  checks passing first, or pushing to main anything but what went live or
  that rebased onto a moved main and built (the push's own run tests the
  rebased tree before it goes live)
- a job acting on text from anyone outside its configured authors
- one machine's details hard-coded into a generic module
