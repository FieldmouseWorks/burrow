"""How a fix job is told to weigh review findings: on their merits, by
burrow-brief's text (tasks.FINDINGS, whose wording burrow-brief's own tests
pin), the same in its first brief and in one past the review-round limit,
stated once and with no second, looser instruction beside it. answers.py
and rounds.py see the same briefs through handle()."""

from harness import H, head
import tasks as T

pr = {"number": 6, "title": "t", "headRefName": "x", "headRefOid": head, "baseRefName": "main", "body": ""}
first = H.fix_task("sam/r", pr, "2 open review threads")
capped = H.fix_task("sam/r", pr, "1 open review thread, after 5 review rounds with findings, the limit, so its "
                    "reviewers won't look again, and a Claude job judges it after, without the reviewers")
# What it's held to is burrow-brief's, with what must be fixed inside it.
assert T.MUST_FIX in T.FINDINGS and "merits" in T.FINDINGS, T.FINDINGS
for brief in (first, capped):
    assert brief.count(T.FINDINGS) == 1, brief
    # Said once: no second, looser instruction beside it.
    assert brief.count("merits") == 1 and "fix what's a real defect" not in brief, brief
