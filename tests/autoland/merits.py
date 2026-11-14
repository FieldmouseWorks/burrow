"""How a fix job is told to weigh review findings (tasks.FINDINGS): on their
merits, the same in its first brief and in one past the review-round limit.
A concrete defect or a break of a rule the base's Code Review Rules list as
P1 is fixed in full, with a regression test for each case; an unreachable
edge, a style preference or an unsupported hypothetical is answered with
reasons; and a P1 label with neither a listed rule nor a concrete failure
behind it counts as P2. answers.py and rounds.py see the same briefs
through handle()."""

from harness import H, head

pr = {"number": 6, "title": "t", "headRefName": "x", "headRefOid": head, "baseRefName": "main", "body": ""}
first = H.fix_task("sam/r", pr, "2 open review threads")
capped = H.fix_task("sam/r", pr, "1 open review thread, after 5 review rounds with findings, the limit, so its "
                    "reviewers won't look again, and a Claude job judges it after, without the reviewers")
for brief in (first, capped):
    for said_so in ("Weigh every finding on its merits, the same from the first round on, past the review-round limit too",
                    # Fixed in full: a concrete defect, or a listed P1 rule broken, by the base's rules.
                    "Fix in full each one that is a concrete defect",
                    "inputs or a state you can name that give a wrong result",
                    "a break of a rule the Code Review Rules of the repo's AGENTS.md, as its base has them, list as P1",
                    "all of what its finding says a complete fix covers",
                    "a regression test for each of those cases that fails without the fix",
                    # Answered with reasons, nothing changed.
                    "Answer the rest with your reasons and change nothing for them",
                    "an edge no input or state can reach", "a style preference no rule asks for",
                    "a hypothetical the finding gives no concrete failure scenario for",
                    # A bare P1 label is no reason to fix.
                    "A finding labelled P1 that names no rule on that P1 list",
                    "no concrete failure scenario counts as P2 here"):
        assert said_so in brief, (said_so, brief)
    # Said once: no second, looser instruction beside it.
    assert brief.count("merits") == 1 and "fix what's a real defect" not in brief, brief
