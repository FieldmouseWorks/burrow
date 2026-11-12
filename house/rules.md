<!-- house:rules v1 begin. Copied from FieldmouseWorks/burrow house/rules.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
## House rules

These hold in every repo. This file's own sections add to them and win where
they're more specific.

- Design the class, not the instance. When a bug, finding or stall shares a
  cause with others, fix the cause once, where it lives (one owning path, a
  type, a check), so none of its cases come back.
- Do work once. Look for what's already open or built first, extend the
  shared crate, client or job that does it rather than write another copy,
  and never run work twice that once would do. The same inputs get the same
  verdict: reuse a proven result rather than run it again, and start heavy
  work once its inputs have settled.
- Fix the whole cause: no workarounds, shims, fallbacks or half-migrations.
  When you replace something, delete the old path in the same change.
- One fact, one home. Each rule or fact lives in one place; link to it
  instead of restating it. Settings too: a doc points at the source and
  never restates a value.
- Before you edit, say what done looks like and how it will be checked.
- Report only the checks you ran and what they said. Skipped, unavailable
  or "should pass" is never a pass. Keep local and CI results apart.
- A new check comes with proof that it fails on the bug it's for.
- The code is the memory. When something goes wrong, make the code, a type,
  a test or a check stop it happening again; a new rule here is the last
  resort. Say in the PR which one you did.
- A model runs only where judgment is needed: a build, proof, replay or poll
  runs as a program, and a model reads its result only when it fails.
- Nothing degrades quietly: a step allowed to fail soft records that it
  degraded, and that's watched like a failure.
- A limit that work keeps hitting is a design bug: split the work or move it
  off the limit, don't raise it.
- Anything that spends (models, runners, money) is off until someone turns it
  on for a repo, and reports what it spent.
- A switch isn't real until something enforces it and a status shows it in
  effect.
- Work goes in order of explicit priority, never by age or accident.
- A person is asked once per decision: a question that's already open is
  joined, not asked again.
- A fix to something Conary will replace records its lesson in Conary's
  lessons file, so the rebuild starts with it.
- A pipeline piece is a reusable module or library any machine can import; a
  machine's flake holds only its own values.
- Leave Nix you touch modular and its glue in Rust: when a change already
  touches a piece of Nix config or the shell or Python glue around it, leave
  it a reusable module any machine could import and move the glue to Rust
  where Rust fits. Only code a change already touches; never a refactor-only
  issue, never a fork of Nix or nixpkgs.
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
- Anything that spends is off by default: a PR that adds an automated spender
  or turns one on by default says `Why on by default:`.
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
