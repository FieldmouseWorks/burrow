"""An engine never judges the head it wrote."""

import json
import subprocess
from unittest.mock import patch

from harness import C, JOBS, clean, fresh_pr_state, head, merged, mkpr, pr_state, queued, run, worker
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

# An unreadable list waits without starting a judge or landing. Repeated
# failures aren't cached; a later pass can read the commits and land normally.
for unreadable in [None, [[{"sha": "moved", "commit": {"message": "change: code"}}]]]:
    fresh_pr_state()
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()

    def unread_commits(*args, **kwargs):
        if args[:1] == ("api",) and args[-1].endswith("/commits?per_page=100"):
            return unreadable
        return real_commit_gh(*args, **kwargs)
    with patch.object(C, "gh", side_effect=unread_commits):
        for at in (650, 710):
            status, _ = run(clean(), at)
            assert "state=pending" in status and "couldn't read its commits; next pass" in status, status
            assert not merged() and not JOBS, (status, JOBS)
            assert "codexHead" not in pr_state()
    run(clean(), 720)
    assert merged() and not JOBS

# Cache both answers across saved-state reloads. Either a new head or a new
# base needs new pages and new trailer parsing, even when the answer is false.
for written in (False, True):
    st = {}
    for sha, base in [(head, "main"), ("b" * 40, "main"), ("b" * 40, "release")]:
        pr = mkpr(sha=sha, base=base)
        pages = [[{"commit": {"message": "change: earlier"}},
                  {"sha": sha, "commit": {"message": "change: code\n\nEngine: codex\n" if written else "change: code"}}]]
        with patch.object(C, "gh", return_value=pages) as read, \
                patch("touches.subprocess.run", wraps=subprocess.run) as parse:
            assert codex_written("sam/r", pr, st) is written
            assert parse.call_count == 2 and read.call_count == 1
            st = json.loads(json.dumps(st))
            assert codex_written("sam/r", pr, st) is written
            assert parse.call_count == 2 and read.call_count == 1

# A read of a moved or truncated list is never cached as this head's own.
for incomplete in [{"sha": "another-head", "commit": {"message": "change: later"}}, {"sha": head, "commit": {}}]:
    C.gh = lambda *a, **k: [[incomplete]]
    st = {}
    assert codex_written("sam/r", mkpr(), st) is None and "commitHead" not in st
C.gh = real_commit_gh
