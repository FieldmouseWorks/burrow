"""Days a PR spent set aside (a hold, a draft, a stack) were someone else's
to end: off one, its day starts again, as the live passes saw it come back,
and a status run (its own state) says the same. Held again and let go, its
day starts from the later release, and a new head pushed since doesn't lose
it. (watched.py has the stuck issue's side of it.)"""

from harness import C, T0, fresh_pr_state, mkpr, pr_state, run, stand_in, status_run

stand_in()
fresh_pr_state()
DAY = 86400
newer = "9" * 40


def listed(digest):
    return any(row[0] == "Open over a day" for row in digest)


def both(pr, at):
    """Whether a live pass, then a status run, list it `at` on."""
    return listed(run(pr, at)[1]), listed(status_run(lambda: run(pr, at + 1))[0][1])


held = mkpr(labels=["hold"], threads=[False])
run(held, 100)
assert not listed(run(held, 2 * DAY)[1])
assert both(mkpr(threads=[False]), 2 * DAY + 100) == (False, False)
assert pr_state()["back"] == C.ts(T0) + 2 * DAY + 100, pr_state()
run(held, 2 * DAY + 200)
assert both(mkpr(threads=[False]), 2 * DAY + 300) == (False, False)
assert both(mkpr(sha=newer, threads=[False]), 3 * DAY) == (False, False)
assert both(mkpr(sha=newer, threads=[False]), 3 * DAY + 400) == (True, True)
print("a day from its release: ok")
