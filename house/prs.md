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
