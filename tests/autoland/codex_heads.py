"""An engine never judges the head it wrote."""

from harness import C, JOBS, fresh_pr_state, head, merged, mkpr, queued, run, worker
from touches import codex_written


# A Codex-written commit on a clean head gets the ordinary reviews, then a
# Claude judge. A quoted Engine line in prose is no trailer and lands as before.
real_commit_gh = C.gh
for message, judge in [("change: code\n\nEngine: codex\n", True),
                       ("change: code\n\neNgInE: codex\n", True),
                       ("change: prose\n\nEngine: codex\n\nThat was a quoted line.\n", False),
                       ("change: code\n", False)]:
    fresh_pr_state()
    def commits(*args, **kwargs):
        got = real_commit_gh(*args, **kwargs)
        if args[:1] == ("api",) and args[-1].endswith("/commits?per_page=100"):
            # The trailer need not be on the last commit of the PR.
            return [[{"commit": {"message": message}}, {"sha": head, "commit": {"message": "change: later"}}]]
        return got
    C.gh = commits
    run(mkpr(), 0)
    run(mkpr(), 600)
    assert queued() == ["codex", "rival"]
    worker()
    status, _ = run(mkpr(), 650)
    assert merged() is not judge, (status, message)
    assert bool(JOBS) is judge, (status, JOBS)
    if judge:
        assert "a Codex job wrote it" in status and "judging" in status, status
C.gh = real_commit_gh

# A read of a moved or truncated list is never cached as this head's own.
for incomplete in [{"sha": "another-head", "commit": {"message": "change: later"}}, {"sha": head, "commit": {}}]:
    C.gh = lambda *a, **k: [[incomplete]]
    st = {}
    assert codex_written("sam/r", mkpr(), st) is None and "commitHead" not in st
C.gh = real_commit_gh
