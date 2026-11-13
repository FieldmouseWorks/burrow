"""A look at an earlier head carries to one that's its change merged onto a
newer base, and nothing else (carry.rs, verdict.rs): a PR whose head moved
only by a merge of its base lands on the looks at the head before it, with
none asked for again; one whose merge edited its own lines is looked at
again, and so is one whose newest look there had findings; and a judge still
looks at what a carried head hides from the reviewers. GitHub's git remote
is a private clone of the bare repo review.py made, and the steward's mirror and
burrow-upkeep's `carry` and `verdict` are the real ones."""

import subprocess

from harness import POSTED, REAL, V, fresh_pr_state, merged, mkpr, queued, run, rv_review, sh, stand_in, tmp, upkeep_config, C, worker

stand_in()
V.carry = REAL["carry"]
# Each scenario advances main, so keep this part's remote private instead
# of rewinding the shared fixture's default branch afterward.
origin_root, work = f"{tmp}/carry-origin", f"{tmp}/carry"
origin = f"{origin_root}/sam/r.git"
sh("git", "clone", "-q", "--bare", f"{tmp}/origin/sam/r.git", origin)
kept_upkeep, C.CFG["upkeep"] = C.CFG["upkeep"], upkeep_config(f"{tmp}/upkeep-carry.json", git=origin_root)
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
sh("git", "clone", "-q", origin, work)
sh("git", "checkout", "-qB", "main", "origin/main", cwd=work)


def lines(at=0, said=""):
    """Twenty numbered lines, line `at` said as `said`."""
    return "".join(f"{said}\n" if i == at else f"line {i}\n" for i in range(1, 21))


def commit(text, msg, path="carry.txt"):
    with open(f"{work}/{path}", "w") as f:
        f.write(text)
    sh(*GIT, "add", path, cwd=work)
    sh(*GIT, "commit", "-qm", msg, cwd=work)
    return sh("git", "rev-parse", "HEAD", cwd=work).strip()


def push(*refs):
    sh("git", "push", "-qf", origin, *refs, cwd=work)


def main_moves_on(text, msg):
    sh("git", "checkout", "-q", "main", cwd=work)
    push(f"{commit(text, msg)}:refs/heads/main")


def merge_main(branch):
    """main merged into `branch`, pushed as PR 9's head."""
    sh("git", "checkout", "-q", branch, cwd=work)
    sh(*GIT, "merge", "-q", "--no-edit", "main", cwd=work)
    h = sh("git", "rev-parse", "HEAD", cwd=work).strip()
    push(f"{h}:refs/pull/9/head")
    return h


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
looked = [rv_review("codex", 0, reviewed), rv_review("rival", 0, reviewed)]

# main moves on in the same file (two lines atop it, which move where the
# PR's edit sits, and a line beside it) and is merged into the PR: a new
# head that's the change they read merged onto it. It lands on their looks,
# with CI green on it, and neither is asked again.
main_moves_on("top 1\ntop 2\n" + lines(13, "main moved on"), "main moves on")
merged_main = merge_main("feat")
status, _ = run(mkpr(number=9, sha=merged_main, reviews=looked), 700)
assert queued() == [] and merged(), (status, queued())
assert "Codex and Rival found nothing" in status, status

# Codex's newest look at the reviewed head had findings: its clean one
# before them doesn't carry, so it looks at this head (Rival's still does).
fresh_pr_state()
since = [*looked, rv_review("codex", 1, reviewed, at="2026-10-24T00:20:00Z")]
status, _ = run(mkpr(number=9, sha=merged_main, reviews=since), 0)
assert not merged() and "Waiting for Codex's review of this commit" in status and "Rival" not in status, status
run(mkpr(number=9, sha=merged_main, reviews=since), 600)
assert queued() == ["codex"] and not merged(), queued()

# main changes the PR's own line instead, and the merge's resolution edits
# it: that's a change nobody has read, so both look again before it lands.
fresh_pr_state()
main_moves_on(lines(15, "main took the line"), "main takes the line")
sh("git", "checkout", "-qb", "resolved", reviewed, cwd=work)
assert subprocess.run([*GIT, "merge", "-q", "--no-edit", "main"], cwd=work, capture_output=True).returncode, "it conflicts"
resolved = commit(lines(15, "the PR's line, and main's"), "resolved")
push(f"{resolved}:refs/pull/9/head")
status, _ = run(mkpr(number=9, sha=resolved, reviews=looked), 0)
assert not merged() and "Waiting for Codex's review of this commit" in status, status
status, _ = run(mkpr(number=9, sha=resolved, reviews=looked), 600)
assert queued() == ["codex", "rival"] and not merged(), (status, queued())

# A PR that changes a path the reviewers aren't shown: their looks carry to
# its merge of main, but no snapshot of that head is taken, so `carry` says
# what it hides, and a judge looks before it lands.
fresh_pr_state()
sh("git", "checkout", "-qb", "keyed", "main", cwd=work)
keyed = commit("SECRET\n", "a key", path="key.pem")
push(f"{keyed}:refs/pull/9/head")
main_moves_on(lines(3, "main again"), "main again")
merged_keyed = merge_main("keyed")
keyed_looks = [rv_review("codex", 0, keyed), rv_review("rival", 0, keyed)]
status, _ = run(mkpr(number=9, sha=merged_keyed, reviews=keyed_looks), 0)
assert queued() == [] and not merged() and "key.pem" in status and "aren't shown" in status, status

C.CFG["upkeep"] = kept_upkeep
stand_in()
fresh_pr_state()
print("carried: ok")
