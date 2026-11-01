# burrow

NixOS modules for a machine you run by talking to Claude. What each module does
is in the [README](README.md); the house style every repo shares, and the
baseline it's held to, are in [docs/](docs/).

<!-- house:rules v1 begin. Copied from FieldmouseWorks/burrow house/rules.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## House rules

These hold in every repo. This file's own sections add to them and win where
they say something more specific.

- Fix the whole cause. No workarounds, shims, fallbacks or half-migrations:
  when you replace something, delete the old path in the same change.
- One fact, one home. Each rule or fact lives in one place; link to it instead
  of restating it.
- Before you edit, say what done looks like and how it will be checked. Simple
  work gets a short plan, not a ceremony.
- Report only the checks you actually ran, and what they said. Skipped,
  unavailable or "should pass" is never a pass. Keep local and CI results
  apart.
- A new check comes with proof that it fails on the bug it's for.
- Split a code file before it passes 1,000 lines. A PR that grows one past
  that, or grows one already over it, splits it, or says why not on a line in
  its body starting `Why not split:`.
- Rust by default, shell included. New code that's meant to last starts in
  Rust unless its platform picks the language, and anything else takes a
  solid reason: a coin toss goes to Rust. New Python or shell says why it
  isn't Rust, on a line in its PR body starting `Why not Rust:` (a PR that
  adds Python, or grows Python and shell past a fix, needs one): a file a
  tool generates, glue that only runs another program, a throwaway script,
  or a tool still being tuned, ported once it settles. Port existing Python
  and shell whenever Rust buys anything, system and pipeline code first, and
  let the port redraw where the code lives and what owns it rather than copy
  it.
- Work starts from an event (a push, a webhook, a unit finishing), not a
  clock. A PR that adds a timer or a cron schedule says why on a line in its
  body starting `Why a timer:`.
- The code is the memory. When something goes wrong, make the code, a type, a
  test or a check stop it happening again; a new rule in AGENTS.md is the last
  resort. Say in the PR which one you did.
- Keep going on work you were asked to do. Don't stop to ask again for what's
  already been asked for.
- Never commit what the repo keeps private: secrets, tokens, personal data, and
  anything listed under this file's "Private" section.
- `Closes #N` only when the issue is fully done; otherwise `Refs #N`.
- Tracked files never name a model. Which model does what is the machine's
  business, and model names go stale in weeks. A line that has to (a recorded
  screen in a test) gets a line in the PR body starting `Why a model name:`.
- CI's house check holds every PR to the rules above that ask for a line in
  its body, with a reason of four words or more after it, and to its title's
  style. Editing the body or title runs it again.

### Commits

Subjects are `area: plain sentence`, where the area is the module, directory or
tool the change is about: `autoland: merge only the head that was checked`.
Lowercase after the colon, no trailing period, no `feat:`/`fix:` prefixes. The
body, when there is one, says why. A PR's title is the subject main gets when
it lands, so it's written the same way.

Keep the git identity your machine gives you. Where commits are signed, the key
belongs to that identity: a signed commit under someone else's name and email
shows as Unverified on GitHub, and CI fails it.
<!-- house:rules end -->

## Build and test

`flake-gate` is the gate, the same here as in CI: it runs the gate step of the
shared `flake-check.yml` every project calls, with the inputs
`.github/workflows/flake.yml` gives it. That's eval-apart, each output
evaluated in a nix process of its own, because evaluating the whole flake at
once holds 6-7G, more than a Claude job on the machine may (a bare
`nix flake check` gets the job killed); and `agents` evaluated but not built,
since CI's runners can't boot it (flake.yml says what builds it instead).
CI splits it across legs, each on a runner of its own, where flake-gate runs
it whole. The
gate builds the example machine, evaluates each module on its own, runs
`lib/project.nix`'s gates on the test projects in `tests/house/`, and builds
the other checks.

One check on its own: `nix build .#checks.x86_64-linux.agents -L` boots the
agent sandboxes (nested KVM: it needs `/dev/kvm`).

The programs burrow's modules run are Rust, in one Cargo workspace,
`crates/`: the flake builds each crate's as a package of its own,
`packages.SYSTEM.CRATE`, which is what a module runs, and gates them as
`checks.crates-*`, and `cargo test` there runs the same tests. The shared
workflows' tests are there too, `crates/burrow-workflows`, which ships
nothing: they run a workflow's own steps, read the files `flake.nix` gives
them by their place in a checkout, and need git, jq and nix on PATH.

burrow is generic. Anything about one machine (its hostname, users, owner,
repos, addresses) is an option a machine's flake sets, never a value in a
module. `examples/` is the machine the checks build.

<!-- house:prs v1 begin. Copied from FieldmouseWorks/burrow house/prs.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## Pull requests

Open PRs ready for review, never as drafts, and leave them to the steward: it
asks the review bot for a review, and once the bot finds nothing on your head,
every review thread is resolved and CI is green, it merges the PR. Don't merge
it yourself. Fix or answer every finding: push the fix and reply with the
commit, or reply with why it's wrong, then resolve the thread. A PR touching a
one-way path (secrets, CODEOWNERS) gets a judgment first: it lands unless it
can't be undone or is the owner's to decide.

A red default branch (CI failed on its tip) is the steward's to fix too: a
Claude job takes it as soon as the steward sees it, and its fix comes from a
branch named `autoland/red-` and the commit (`gh pr list --search
head:autoland/red-`). Don't fix it yourself, in any repo: it needs one fix,
not several racing each other. Only if the pipeline's stuck issue says the
steward's jobs gave up on it, fix it, from a branch of that name, so the
steward knows it for the fix.
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
- the steward merging what it shouldn't: a head other than the one its
  reviewers and CI checked, a one-way path, or a PR with open threads
- a review agent that reaches more than its snapshot: a GitHub token, a
  secrets path (`reviewers.skipPaths`), the PR's own copy of its review
  rules, or its own key in what gets posted
- `HOST-update` going live or confirming without the VM tests and health
  checks passing first, or pushing to main anything but what went live or
  that rebased onto a moved main and built (the push's own run tests the
  rebased tree before it goes live)
- a job acting on text from anyone outside its configured authors
- one machine's details hard-coded into a generic module
