"""The digest's two issues: what only the owner can settle goes to the issue
assigned to them, and what the pipeline is stuck on to one the inbox hands to
a Claude job; without an inbox, all of it is the owner's."""

import os

from harness import C, R, fresh_pr_state, tmp

CALLS = []
OPEN = {}  # the open issue under each label, as `gh issue list` finds it


def gh(*a, **k):
    CALLS.append(a)
    if a[:2] == ("issue", "list"):
        n = OPEN.get(a[a.index("--label") + 1])
        return [{"number": n, "body": ""}] if n else []
    return {}


def made(label):
    """The `gh issue create` under label, as {flag: value}."""
    got = [a for a in CALLS if a[:2] == ("issue", "create") and a[a.index("--label") + 1] == label]
    assert len(got) <= 1, got
    return dict(zip(got[0][2::2], got[0][3::2])) if got else None


def fresh(inbox):
    CALLS.clear()
    OPEN.clear()
    fresh_pr_state()
    R.CFG["inbox"] = inbox


kept = (C.gh, R.CFG["inbox"], dict(R.CFG["notify"]))
C.gh = gh
R.CFG["notify"].update(repo="sam/notes", assignee="sam")
DIGEST = [
    ("Outside contributions", "sam/r#1", "Outside contribution: Sam reviews and merges it"),
    ("Labelled needs-owner", "sam/r#2", ""),
    ("Stale branches (unmerged, no PR, 30+ days)", "sam/r: `old`", ""),
    ("Rival couldn't review it", "sam/r#3", "rival exited 1 (journalctl -u autoland-review)"),
    ("Couldn't merge", "sam/r#4", "gh pr merge keeps failing (journalctl -u autoland)"),
    ("Two Claude jobs ended without fixing it", "sam/r#5", "its findings"),
]

# With an inbox: the owner's issue lists only their calls, assigned to them;
# the rest is a Claude job's, labelled for the inbox and assigned to nobody.
fresh("claude")
R.publish(DIGEST)
owner, stuck = made("autoland"), made("claude")
assert owner and owner["--title"] == "[autoland] PRs waiting for you" and owner["--assignee"] == "sam", CALLS
assert all(x in owner["--body"] for x in ("sam/r#1", "sam/r#2", "`old`")), owner["--body"]
assert not any(x in owner["--body"] for x in ("sam/r#3", "sam/r#4", "sam/r#5")), owner["--body"]
assert stuck and stuck["--title"] == "[autoland] Stuck in the pipeline" and "--assignee" not in stuck, CALLS
assert stuck["--body"].startswith(R.stuck_brief()) and f"{tmp}/state" in stuck["--body"], stuck["--body"]
assert all(x in stuck["--body"] for x in ("sam/r#3", "sam/r#4", "sam/r#5")), stuck["--body"]
assert not any(x in stuck["--body"] for x in ("sam/r#1", "sam/r#2", "`old`")), stuck["--body"]

# Every kind the owner settles is one the steward lists: a renamed one
# would go to the inbox, never to the owner.
src = "".join(open(os.path.join(os.path.dirname(R.__file__), f)).read() for f in ("steward.py", "repos.py"))
for kind in R.OWNERS:
    assert src.count(f'"{kind}"') >= 2, kind

# Unstuck: its issue closes, and the owner's stays as it was.
fresh("claude")
R.publish(DIGEST)
OPEN.update(autoland=7, claude=8)
CALLS.clear()
C.NOW += 1
R.publish(DIGEST[:3])
assert ("issue", "close", "8", "-R", "sam/notes", "--comment", "Nothing stuck.") in CALLS, CALLS
assert not any(a[:2] in (("issue", "edit"), ("issue", "comment"), ("issue", "close")) and a[2] == "7" for a in CALLS), CALLS
# Stuck again: the line is new to its issue, and comes as a comment.
OPEN.pop("claude")
R.publish(DIGEST[:4])
OPEN["claude"] = 9
R.publish(DIGEST)
new = [a for a in CALLS if a[:3] == ("issue", "comment", "9")]
assert len(new) == 1 and "sam/r#4" in new[0][-1] and "sam/r#3" not in new[0][-1], new

# Without an inbox, all of it is the owner's.
fresh("")
R.publish(DIGEST)
owner = made("autoland")
assert owner and all(f"sam/r#{n}" in owner["--body"] for n in range(1, 6)), owner
assert not any(a[:2] == ("issue", "create") and "--title" in a and "Stuck" in a[a.index("--title") + 1] for a in CALLS)
assert not any(a[:2] == ("issue", "list") and "claude" in a for a in CALLS), CALLS

C.gh, R.CFG["inbox"] = kept[:2]
R.CFG["notify"].clear()
R.CFG["notify"].update(kept[2])
fresh_pr_state()
print("digest: ok")
