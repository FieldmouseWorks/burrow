"""A clean look carries to a head whose own change is the same (verdict.rs,
change.rs): a PR whose head moved only by a merge of its base lands on the
looks at the head before it, with none asked for again, and one whose merge
edited its own lines is looked at again. GitHub's git remote is the bare
repo review.py made, and the steward's mirror and burrow-upkeep's `change`
and `verdict` are the real ones."""

import subprocess

from harness import (MARKED, POSTED, REAL, V, fresh_pr_state, merged, mkpr, queued, run, rv_review, sh, stand_in, tmp,
                     worker)

stand_in()
V.change = REAL["change"]
origin, work = f"{tmp}/origin/sam/r.git", f"{tmp}/carry"
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
sh("git", "clone", "-q", origin, work)
sh("git", "checkout", "-qB", "main", "origin/main", cwd=work)
was = sh("git", "rev-parse", "HEAD", cwd=work).strip()  # main as the parts before left it, put back after


def lines(at=0, said=""):
    """Twenty numbered lines, line `at` said as `said`."""
    return "".join(f"{said}\n" if i == at else f"line {i}\n" for i in range(1, 21))


def commit(text, msg):
    with open(f"{work}/carry.txt", "w") as f:
        f.write(text)
    sh(*GIT, "add", "carry.txt", cwd=work)
    sh(*GIT, "commit", "-qm", msg, cwd=work)
    return sh("git", "rev-parse", "HEAD", cwd=work).strip()


def push(*refs):
    sh("git", "push", "-qf", origin, *refs, cwd=work)


# PR 9 changes the 15th line of a file on main. Both reviewers find nothing
# on its head while CI is still at work, so it doesn't land yet.
push(f"{commit(lines(), 'twenty lines')}:refs/heads/main")
sh("git", "checkout", "-qb", "feat", cwd=work)
reviewed = commit(lines(15, "the PR's line"), "the PR")
push(f"{reviewed}:refs/pull/9/head")
busy = [{"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": None}]
fresh_pr_state()
run(mkpr(number=9, sha=reviewed, checks=busy), 0)
run(mkpr(number=9, sha=reviewed, checks=busy), 600)
assert queued() == ["codex", "rival"], queued()
worker()
status, _ = run(mkpr(number=9, sha=reviewed, checks=busy), 650)
assert sorted(POSTED) == [("codex", reviewed, 0), ("rival", reviewed, 0)] and not merged(), (POSTED, status)
# Each review is marked with the PR's own change at the head it read.
change = MARKED[0]
assert isinstance(change, str) and len(change) == 64 and MARKED == [change, change], MARKED
looked = [rv_review("codex", 0, reviewed, change=change), rv_review("rival", 0, reviewed, change=change)]

# main moves on in the same file (two lines atop it, which move where the
# PR's hunk sits, and a line in that hunk's context) and is merged into the
# PR: a new head, whose own change is the one they read. It lands on their
# looks, with CI green on it, and neither is asked again.
sh("git", "checkout", "-q", "main", cwd=work)
moved_on = "top 1\ntop 2\n" + lines(12, "main moved on")
push(f"{commit(moved_on, 'main moves on')}:refs/heads/main")
sh("git", "checkout", "-q", "feat", cwd=work)
sh(*GIT, "merge", "-q", "--no-edit", "main", cwd=work)
merged_main = sh("git", "rev-parse", "HEAD", cwd=work).strip()
push(f"{merged_main}:refs/pull/9/head")
status, _ = run(mkpr(number=9, sha=merged_main, reviews=looked), 700)
assert queued() == [] and merged(), (status, queued())
assert "Codex and Rival found nothing" in status, status

# main changes the PR's own line instead, and the merge's resolution edits
# it: that's a change nobody has read, so both look again before it lands.
fresh_pr_state()
sh("git", "checkout", "-q", "main", cwd=work)
push(f"{commit(lines(15, 'main took the line'), 'main takes the line')}:refs/heads/main")
sh("git", "checkout", "-qb", "resolved", reviewed, cwd=work)
assert subprocess.run([*GIT, "merge", "-q", "--no-edit", "main"], cwd=work, capture_output=True).returncode, "it conflicts"
resolved = commit(lines(15, "the PR's line, and main's"), "resolved")
push(f"{resolved}:refs/pull/9/head")
status, _ = run(mkpr(number=9, sha=resolved, reviews=looked), 0)
assert not merged() and "Waiting for Codex's review of this commit" in status, status
status, _ = run(mkpr(number=9, sha=resolved, reviews=looked), 600)
assert queued() == ["codex", "rival"] and not merged(), (status, queued())

push(f"{was}:refs/heads/main", ":refs/pull/9/head")
stand_in()
fresh_pr_state()
print("carried: ok")
