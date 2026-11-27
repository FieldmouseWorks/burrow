<!-- house:review v1 begin. Copied from FieldmouseWorks/burrow house/review.md, verbatim. Change it there, not here: burrow's house sync rewrites everything between these markers. -->
For reviewers, the review bot included.

Not P1: flag a PR that gives a file a second job, at any length. The author
splits it or answers with a reason; a fix job is not required to split a
file mid-PR.

Flag as P1:

- a change that makes a check pass without fixing the thing it checks
- tests or checks weakened, skipped or deleted to get green
- secrets, tokens or personal data in the diff or the logs it adds
- behavior changed beyond what the PR says it does
<!-- house:review end -->
