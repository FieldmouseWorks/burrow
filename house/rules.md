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
