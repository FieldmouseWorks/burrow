"""An engine never judges the head it wrote."""

import json
import subprocess
from unittest.mock import patch

from harness import C, JOBS, S, clean, commit, compare_pages, fresh_pr_state, head, merged, mkpr, pr_state, queued, run, worker
from touches import DEPENDABOT_EMAIL, codex_written, provenance


def paged(a):
    """A paged read of a PR's commits, as gh is asked for it."""
    return a[:3] == ("api", "--paginate", "--slurp") and "/compare/" in a[3]


# A Codex-written commit on a clean head gets the ordinary reviews, then a
# Claude judge. A quoted Engine line in prose is no trailer and lands as before.
real_commit_gh = C.gh
for message, judge in [("change: code\n\nEngine: codex\n", True),
                       ("change: code\n\neNgInE: codex\n", True),
                       ("change: code\n\nOriginal-engine: codex\n", False),
                       ("change: code\n\nNote: Engine: codex is an example\n", False),
                       ("change: code\n\nEngine: codex, once\n", False),
                       ("change: prose\n\nEngine: codex\n\nThat was a quoted line.\n", False),
                       ("change: code\n", False)]:
    fresh_pr_state()

    def commits(*args, **kwargs):
        got = real_commit_gh(*args, **kwargs)
        if paged(args):
            # The trailer need not be on the last commit of the PR.
            return compare_pages([commit("e" * 40, message), commit(head, "change: later")])
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

# An unreadable list waits without starting a judge or landing: a failed
# read, a list without the head, or pages that disagree because the base
# moved between them. Repeated failures aren't cached; a later pass can read
# the commits and land normally.
moved = compare_pages([commit("%040x" % i, "change: code") for i in range(150)] + [commit(head, "change: code")])
for unreadable in [None, compare_pages([commit("b" * 40, "change: code")]),
                   [moved[0], {**moved[1], "merge_base_commit": {"sha": "d" * 40}}],
                   [moved[0], {**moved[1], "total_commits": 152}]]:
    fresh_pr_state()
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()

    def unread_commits(*args, **kwargs):
        if paged(args):
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
        pages = compare_pages([commit("e" * 40, "change: earlier"),
                               commit(sha, "change: code\n\nEngine: codex\n" if written else "change: code")])
        with patch.object(C, "gh", return_value=pages) as read, \
                patch("touches.subprocess.run", wraps=subprocess.run) as parse:
            assert codex_written("sam/r", pr, st) is written
            assert parse.call_count == 2 and read.call_count == 1
            st = json.loads(json.dumps(st))
            assert codex_written("sam/r", pr, st) is written
            assert parse.call_count == 2 and read.call_count == 1

# A read of another head's list, or of a commit with no message, is never
# cached as this head's own.
for incomplete in [commit("another-head", "change: later"), {"sha": head, "commit": {}}]:
    C.gh = lambda *a, **k: compare_pages([incomplete])
    st = {}
    assert codex_written("sam/r", mkpr(), st) is None and "commitsHead" not in st
C.gh = real_commit_gh


def github(commits, shown=None):
    """GitHub as gh reads a PR's commits: its PR commit list stops at 250,
    however it's paged; its compare, paged, lists the whole range, or only
    the first `shown` where GitHub stops short of its own count."""
    def gh(*a, **k):
        if a[:1] == ("api",) and a[-1].endswith(f"/pulls/{mkpr()['number']}/commits?per_page=100"):
            return [commits[:250][i:i + 100] for i in range(0, 250, 100)]
        if paged(a):
            return compare_pages(commits, shown=shown)
        return real_commit_gh(*a, **k)
    return gh


# A PR past GitHub's 250-commit list is read whole: a Codex trailer on its
# 251st commit, the head, counts, and one with none lands on its reviews.
many = [commit("%040x" % i, "change: earlier") for i in range(250)]
for message, written in [("change: code\n\nEngine: codex\n", True), ("change: code", False)]:
    st = {}
    with patch.object(C, "gh", side_effect=github(many + [commit(head, message)])):
        assert codex_written("sam/r", mkpr(), st) is written, st
    fresh_pr_state()
    with patch.object(C, "gh", side_effect=github(many + [commit(head, message)])):
        run(mkpr(), 0)
        run(mkpr(), 600)
        worker()
        status, _ = run(clean(), 650)
    assert merged() is not written and bool(JOBS) is written, (status, JOBS)

# So are its authors, co-authors and committers: a Claude job's co-author
# line on a Dependabot branch's 251st commit is an edit.
bot = [commit("%040x" % i, "deps: bump", DEPENDABOT_EMAIL, "noreply@github.com") for i in range(250)]
dependabot = mkpr(branch="dependabot/npm/foo-2", title="Bump foo from 1.4 to 1.5")
for trailer, edited in [("", False), ("\n\nCo-authored-by: Claude <claude@remi>\n", True)]:
    last = commit(head, "deps: bump" + trailer, DEPENDABOT_EMAIL, "noreply@github.com")
    with patch.object(C, "gh", side_effect=github(bot + [last])):
        assert provenance("sam/r", dependabot, {}) is edited

# A missing or empty address is never a bot's, on a machine with no Renovate
# address too: an addressless co-author line, or a commit with no author or
# committer address, is an edit; the bots' own commits still aren't.
email = S.CFG["renovateEmail"]
S.CFG["renovateEmail"] = None
for author, committer, trailer, edited in [
        (DEPENDABOT_EMAIL, "noreply@github.com", "", False),
        (DEPENDABOT_EMAIL, "noreply@github.com", "\n\nCo-authored-by: Claude\n", True),
        (DEPENDABOT_EMAIL, "noreply@github.com", "\n\nCo-authored-by: Claude <>\n", True),
        (None, "noreply@github.com", "", True),
        ("", "noreply@github.com", "", True),
        (DEPENDABOT_EMAIL, None, "", True)]:
    last = commit(head, "deps: bump" + trailer, author, committer)
    with patch.object(C, "gh", side_effect=github(bot + [last])):
        assert provenance("sam/r", dependabot, {}) is edited, (author, committer, trailer)
S.CFG["renovateEmail"] = email

# Where GitHub stops short of its own count for good (the head and merge
# base fixed, the range can't change), no one can say a Codex job didn't
# write it: it's judged, never landed on its reviews alone, its authorship is
# unknown, and the answer is kept for the head rather than asked every pass.
short = github(many + [commit("%040x" % (250 + i), "change: later") for i in range(49)] + [commit(head, "change: code")], 250)
with patch.object(C, "gh", side_effect=short) as read:
    st = {}
    why = codex_written("sam/r", mkpr(), st)
    assert isinstance(why, str) and "250 of its 300 commits" in why, why
    assert codex_written("sam/r", mkpr(), json.loads(json.dumps(st))) == why and read.call_count == 1
    assert provenance("sam/r", dependabot, {}) is None
fresh_pr_state()
with patch.object(C, "gh", side_effect=short):
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()
    status, _ = run(clean(), 650)
assert not merged() and JOBS and "250 of its 300 commits" in status, (status, JOBS)
