"""The digest's two issues: what only the owner can settle goes to the issue
assigned to them, and what the pipeline is stuck on to one the inbox hands to
a Claude job; when the inbox can't take it, all of it is the owner's."""

import os

from harness import B, C, R, fresh_pr_state, sh, tmp

CALLS = []
OPEN = {}  # the open issue under each label, as `gh issue list` finds it
LABELS = {}  # the labels on each open issue, by number, when not the two the stuck one is made with
GH = {"fail": ""}  # a write that fails: an issue command, or a flag


def gh(*a, **k):
    CALLS.append(a)
    if a[:2] == ("issue", "list"):
        label = a[a.index("--label") + 1]
        n = OPEN.get(label)
        return [{"number": n, "body": "", "labels": [{"name": x} for x in LABELS.get(n, [label, "unstick"])]}] if n else []
    # A write that fails: an issue command, or any call with a flag.
    return None if GH["fail"] and (a[:2] == ("issue", GH["fail"]) or GH["fail"] in a) else {}


def made(label):
    """The `gh issue create` under label, as {flag: value}, its labels as a list."""
    got = [a for a in CALLS if a[:2] == ("issue", "create") and a[a.index("--label") + 1] == label]
    assert len(got) <= 1, got
    if not got:
        return None
    flags = dict(zip(got[0][2::2], got[0][3::2]))
    flags["--label"] = [v for f, v in zip(got[0][2::2], got[0][3::2]) if f == "--label"]
    return flags


def relabelled(n):
    """The labels added to issue n since CALLS was cleared."""
    return [a[a.index("--add-label") + 1] for a in CALLS if a[:3] == ("issue", "edit", str(n)) and "--add-label" in a]


def said(n):
    """The comments on issue n since CALLS was cleared."""
    return [a[a.index("--body") + 1] for a in CALLS if a[:3] == ("issue", "comment", str(n))]


def fresh(enable=True, app=True):
    """With app, the steward's App is one of the inbox's authors, as autoland.nix makes it."""
    CALLS.clear()
    OPEN.clear()
    LABELS.clear()
    fresh_pr_state()
    GH.update(fail="")
    R.CFG["inbox"] = {"enable": enable, "label": "claude", "unstickLabel": "unstick",
                      "authors": ["sam"] + (["steward[bot]"] if app else [])}


def later(digest):
    """A publish an hour on, when even an unchanged digest is looked at again."""
    C.NOW += 3601
    CALLS.clear()
    R.publish(digest)


kept = (C.gh, R.CFG["inbox"], dict(R.CFG["notify"]), R.CFG["projectsDir"])
C.gh = gh
R.CFG["notify"].update(repo="sam/notes", assignee="sam")
# notify.repo cloned in projectsDir, where proj-inbox looks.
R.CFG["projectsDir"] = f"{tmp}/projects"
os.makedirs(f"{tmp}/projects/notes")
sh("git", "init", "-q", f"{tmp}/projects/notes")
sh("git", "-C", f"{tmp}/projects/notes", "remote", "add", "origin", "https://github.com/sam/notes.git")
DIGEST = [
    ("Outside contributions", "sam/r#1", "Outside contribution: Sam reviews and merges it"),
    ("Labelled needs-owner", "sam/r#2", ""),
    ("Judged yours to decide", "sam/r#7", "a Claude job judged it Sam's"),
    ("Rival couldn't review it", "sam/r#3", "rival exited 1 (journalctl -u autoland-review)"),
    ("Couldn't merge", "sam/r#4", "GitHub keeps refusing the merge (journalctl -u autoland)"),
    ("Two Claude jobs ended without fixing it", "sam/r#5", "its findings"),
]

# The inbox takes it: the owner's issue lists only their calls, assigned to
# them; the rest is a Claude job's, labelled for the inbox and assigned to
# nobody.
fresh()
R.publish(DIGEST)
owner, stuck = made("autoland"), made("claude")
assert owner and owner["--title"] == "[autoland] PRs waiting for you" and owner["--assignee"] == "sam", CALLS
assert all(x in owner["--body"] for x in ("sam/r#1", "sam/r#2", "sam/r#7")), owner["--body"]
assert not any(x in owner["--body"] for x in ("sam/r#3", "sam/r#4", "sam/r#5", "Stuck")), owner["--body"]
assert stuck and stuck["--title"] == "[autoland] Stuck in the pipeline" and "--assignee" not in stuck, CALLS
# Its job never waits in proj's line, which may be what's stuck: the inbox
# runs it --unstick for the second label. The owner's has only its own.
assert stuck["--label"] == ["claude", "unstick"] and owner["--label"] == ["autoland"], (stuck, owner)
assert ("label", "create", "unstick", "-R", "sam/notes", "--color", "B60205") in CALLS, CALLS
assert stuck["--body"].startswith(R.stuck_brief()) and f"{tmp}/state" in stuck["--body"], stuck["--body"]
# The kick it gives works from a job's shell: burrow-events starts the
# unit, which alone holds the App's key (`autoland-steward kick` there can't sign).
assert f"curl -fsS -d '' {R.CFG['kick']}/OWNER/REPO" in stuck["--body"] and "autoland-steward kick" not in stuck["--body"], stuck["--body"]
assert all(x in stuck["--body"] for x in ("sam/r#3", "sam/r#4", "sam/r#5")), stuck["--body"]
assert not any(x in stuck["--body"] for x in ("sam/r#1", "sam/r#2", "sam/r#7")), stuck["--body"]

# A stale branch the sweep couldn't archive is the pipeline's trouble, not
# the owner's call: archived, it would have gone on its own.
assert B.UNKEPT not in R.OWNERS and B.UNDELETED not in R.OWNERS

# Every kind the owner settles is one the steward lists: a renamed one
# would go to the inbox, never to the owner.
src = "".join(open(os.path.join(os.path.dirname(R.__file__), f)).read() for f in ("step.py", "review_step.py", "repos.py", "flakes.py"))
for kind in R.OWNERS:
    assert src.count(f'"{kind}"') >= 2, kind

# Unstuck: its issue closes, and the owner's stays as it was.
OPEN.update(autoland=7, claude=8)
CALLS.clear()
C.NOW += 1
R.publish(DIGEST[:3])
assert ("issue", "close", "8", "-R", "sam/notes", "--comment", "Nothing stuck.") in CALLS, CALLS
assert not any(a[:2] in (("issue", "edit"), ("issue", "comment"), ("issue", "close")) and a[2] == "7" for a in CALLS), CALLS
# Stuck again: what's new to its issue comes as a comment, which is what
# hands it to the job there.
OPEN.pop("claude")
R.publish(DIGEST[:4])
OPEN["claude"] = 9
CALLS.clear()
R.publish(DIGEST)
assert said(9) == ["New:\n- Couldn't merge: sam/r#4: GitHub keeps refusing the merge (journalctl -u autoland)\n"
                   "- Two Claude jobs ended without fixing it: sam/r#5: its findings"], said(9)
# It had the unstick label all along: nothing to put back. One open from
# before it had one, or that lost it, gets it back, before the news that
# hands it on, which only comes once that went through.
assert relabelled(9) == [], CALLS
LABELS[9] = ["claude"]
CALLS.clear()
GH["fail"] = "--add-label"
later(DIGEST + [("Open over a day", "sam/r#9", "unsettled")])
assert relabelled(9) == ["unstick"] and said(9) == [], CALLS
GH["fail"] = ""
CALLS.clear()
R.publish(DIGEST + [("Open over a day", "sam/r#9", "unsettled")])
assert relabelled(9) == ["unstick"] and said(9) == ["New:\n- Open over a day: sam/r#9: unsettled"], CALLS
LABELS.pop(9)
R.publish(DIGEST)
# A PR already listed, listed again for something else: news too, kind and
# diagnosis with it.
CALLS.clear()
R.publish(DIGEST + [("Open over a day", "sam/r#4", "unsettled")])
assert said(9) == ["New:\n- Open over a day: sam/r#4: unsettled"], said(9)
# The same kind on the same PR with another diagnosis: news.
moved = [x if x[0] != "Couldn't merge" else (x[0], x[1], "GitHub says it's behind") for x in DIGEST]
CALLS.clear()
R.publish(moved)
assert said(9) == ["New:\n- Couldn't merge: sam/r#4: GitHub says it's behind"], said(9)
# Nothing changed: no comment, even when it's looked at again.
later(moved)
assert said(9) == [], CALLS
# A comment GitHub doesn't take is tried again, and once only.
GH["fail"] = "comment"
again = moved + [("A review couldn't be posted", "sam/r#6", "Rival, 3 tries")]
later(again)
assert said(9) == ["New:\n- A review couldn't be posted: sam/r#6: Rival, 3 tries"], CALLS
GH["fail"] = ""
CALLS.clear()
R.publish(again)
assert said(9) == ["New:\n- A review couldn't be posted: sam/r#6: Rival, 3 tries"], CALLS
later(again)
assert said(9) == [], CALLS

# The inbox can't take it: the steward's App isn't one of its authors, so
# the job would never start. All of it is the owner's, with why, and a stuck
# issue still open closes.
fresh(app=False)
OPEN["claude"] = 9
R.publish(DIGEST)
owner = made("autoland")
assert owner and all(f"sam/r#{n}" in owner["--body"] for n in range(1, 6)), owner
assert "the steward's login, steward[bot], isn't one of its authors" in owner["--body"], owner["--body"]
assert made("claude") is None and ("issue", "close", "9", "-R", "sam/notes", "--comment", "Nothing stuck.") in CALLS, CALLS
# Nor when notify.repo isn't cloned where proj-inbox looks; and the clone
# going away later is seen on the next pass.
fresh()
R.CFG["projectsDir"] = f"{tmp}/none"
R.publish(DIGEST)
owner = made("autoland")
assert owner and f"sam/notes isn't cloned in {tmp}/none" in owner["--body"] and "sam/r#4" in owner["--body"], owner
assert made("claude") is None, CALLS
R.CFG["projectsDir"] = f"{tmp}/projects"
# With nothing stuck, there's nothing to say about the inbox.
fresh(app=False)
R.publish(DIGEST[:3])
assert "proj's inbox" not in made("autoland")["--body"], CALLS
# Off: all of it is the owner's, and that's no misconfiguration.
fresh(enable=False)
R.publish(DIGEST)
owner = made("autoland")
assert owner and all(f"sam/r#{n}" in owner["--body"] for n in range(1, 6)), owner
assert "proj's inbox" not in owner["--body"] and made("claude") is None, owner
# No notify.repo: the digest goes to the journal, with a project cloned and
# the inbox on, and the stuck half is the owner's there too.
LOGGED = []
log_kept, R.log = R.log, lambda *a: LOGGED.append(" ".join(map(str, a)))
for nrepo in (None, ""):
    fresh()
    LOGGED.clear()
    R.CFG["notify"]["repo"] = nrepo
    R.publish(DIGEST)
    assert not [a for a in CALLS if a[0] == "issue"], CALLS
    mine = next(x for x in LOGGED if x.startswith("--- [autoland] PRs waiting for you ---"))
    assert all(f"sam/r#{n}" in mine for n in range(1, 6)) and "notify.repo isn't set" in mine, LOGGED
    assert "--- [autoland] Stuck in the pipeline ---\n(nothing)" in LOGGED, LOGGED
R.log = log_kept
R.CFG["notify"]["repo"] = "sam/notes"

C.gh, R.CFG["inbox"], R.CFG["projectsDir"] = kept[0], kept[1], kept[3]
R.CFG["notify"].clear()
R.CFG["notify"].update(kept[2])
fresh_pr_state()
print("digest: ok")
