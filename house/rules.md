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
- Fix the whole cause: no workarounds, shims, silent fallbacks that hide a
  failure, or untracked half-migrations. When you replace something, delete
  the old path in the same change; a staged migration names the issue that
  ends it.
- One fact, one home. Each rule or fact lives in one place; link to it
  instead of restating it.
- Before you edit, say what done looks like and how it will be checked.
- Keep your context small: every step re-sends all you've read, and a
  long wait makes it pay again. Read a file you're about to edit in full
  unless it's large; for everything else read the lines you need (`rg -n`,
  ranges, a diff's stat then its hunks), not whole logs; cap command output;
  hand broad searches to a subagent; don't re-read what you have.
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

- Rust by default for tools, scripts and services, shell included: new code
  meant to last starts in Rust unless its platform picks the language, and a
  coin toss goes to Rust. A PR that adds Python, or grows Python and shell
  past a fix, says `Why not Rust:` (a file a tool generates, glue that only
  runs another program, a throwaway script, a tool still being tuned) unless
  the path has a standing waiver in `.github/workflows/house.yml`. Port
  existing Python and shell whenever Rust buys anything, system and pipeline
  code first.
- Work starts from an event (a push, a webhook, a unit finishing), not a
  clock. A PR that adds a timer or a cron schedule says `Why a timer:`.
- Split a non-test code file before it passes 1,000 lines (test files and
  inline `#[cfg(test)]` blocks don't count), or say `Why not split:` unless
  the path has a standing waiver in `.github/workflows/house.yml`.
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
