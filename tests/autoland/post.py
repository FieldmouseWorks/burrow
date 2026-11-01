"""What gets posted from a reviewer's answer, by burrow-upkeep's `post` (the
real one, with a gh that answers as GitHub would and notes what it was
asked), and how its verdict is read back. What goes where in the review is
post.rs's to check; this checks what the pass hands it and what it makes of
each outcome."""

import contextlib
import io
import json
import os
import subprocess
import sys

from harness import REAL_GH, C, V, tmp, upkeep_config

kept_upkeep = C.CFG["upkeep"]
C.CFG["upkeep"] = upkeep_config(f"{tmp}/upkeep-post.json")
GH_CALLS, GH_SAYS = f"{tmp}/post-gh-calls", f"{tmp}/post-gh.json"
os.makedirs(f"{tmp}/post-bin")
with open(f"{tmp}/post-bin/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
a = sys.argv[1:]
method, path = a[3], a[4]
body = json.loads(sys.stdin.read()) if "--input" in a else None
with open({GH_CALLS!r}, "a") as f:
    f.write(json.dumps([method, path, body, os.environ.get("GH_TOKEN")]) + "\\n")
says = json.load(open({GH_SAYS!r}))
part = "compare" if "/compare/" in path else path.rsplit("/", 1)[1]
status, got = says.get(part, (404, {{"message": "Not Found"}}))
sys.stdout.write(f"HTTP/1.1 {{status}} X\\r\\n\\r\\n" + json.dumps(got))
sys.exit(0 if status < 300 else 1)
""")
os.chmod(f"{tmp}/post-bin/gh", 0o755)
FILES = {"files": [{"filename": "src/app.py", "status": "modified", "patch": "@@ -10,2 +10,3 @@\n a = 1\n+b = 2\n c = 3"},
                   {"filename": "gone.txt", "status": "removed", "patch": "@@ -1 +0,0 @@\n-bye"}]}


def github(**says):
    """GitHub, from now on: the compare, and what it does with a review and
    a thread, by default the compare above and both taken."""
    with open(GH_SAYS, "w") as f:
        json.dump({"compare": (200, FILES), "reviews": (200, {"id": 1}), "comments": (201, {"id": 2}), **says}, f)
    open(GH_CALLS, "w").close()


def calls():
    return [json.loads(x) for x in open(GH_CALLS)]


kept_path, os.environ["PATH"] = os.environ["PATH"], f"{tmp}/post-bin{os.pathsep}{os.environ['PATH']}"
head = "a" * 40
pr = {"number": 7, "headRefOid": head, "baseRefName": "release/1.0", "title": "Add b", "url": "u", "reviews": {"nodes": []},
      "body": "Adds b.\n\nWhy not Rust: a throwaway script, deleted once it's run"}
shown = C.fingerprint(pr["body"])  # the description the reviews were shown
answer = {"findings": [
    {"path": "src/app.py", "line": 99, "severity": "P0", "title": "ping @evil", "body": "bad"},
    {"path": "gone.txt", "line": 1, "severity": "P2", "title": "t", "body": "b"}], "summary": "s"}
# The review goes as the App's installation on the repo's owner, on the
# head the reviewer read, of its base, named as the steward names it.
github()
with C.acting_for("sam/r"):
    assert V.post("sam/r", pr, "rival", answer, shown) == "findings"
got = calls()
assert [(m, p, t) for m, p, _, t in got] == [
    ("GET", f"repos/sam/r/compare/release%2F1.0...{head}", "ghs_app_sam"), ("POST", "repos/sam/r/pulls/7/reviews", "ghs_app_sam"),
    ("POST", "repos/sam/r/pulls/7/comments", "ghs_app_sam")], got
posted = got[1][2]
assert posted["commit_id"] == head and [c["line"] for c in posted["comments"]] == [12], posted
assert posted["body"].startswith("**Rival review** of aaaaaaa: 2 findings.") and got[2][2]["path"] == "gone.txt", posted
assert V.finding("rival") in got[2][2]["body"] and "@​evil" in posted["comments"][0]["body"], got

# Its verdict comes back from the review, and only from one the steward's
# App posted: not a user's, even one whose login is the App's name, which
# GraphQL gives the App's bot too, without "[bot]".
pr["baseRefName"] = "main"
github()
with C.acting_for("sam/r"):
    assert V.post("sam/r", pr, "rival", answer, shown) == "findings"
posted = calls()[1][2]
assert '"findings":2' in posted["body"], posted["body"]
mine = {"author": {"login": "steward", "__typename": "Bot"}, "body": posted["body"], "commit": {"oid": head},
        "submittedAt": "2026-10-24T00:00:00Z", "state": "COMMENTED"}
forged = dict(mine, author={"login": "mallory", "__typename": "User"}, submittedAt="2026-10-24T01:00:00Z",
              body=posted["body"].replace('"findings":2', '"findings":0'))
namesake = dict(forged, author={"login": "steward", "__typename": "User"})
pr["reviews"]["nodes"] = [mine, forged, namesake]
assert V.verdict(pr, head, "rival")[0::2] == ("findings", 1)
assert V.verdict(pr, "b" * 40, "rival")[0] == "none" and V.verdict(pr, head, "codex")[0] == "none"
# Retargeted, the same head is another change: what was said on main doesn't count.
assert V.verdict(dict(pr, baseRefName="release"), head, "rival")[0] == "none"
# It's a verdict on the description it was shown too: one edited since (a
# reason written in after a clean review, for CI) has had nobody weigh it,
# and one whose review doesn't say what it was shown is no verdict at all.
assert f'"description":"{shown}"' in posted["body"], posted["body"]
edited = dict(pr, body=pr["body"].replace("a throwaway script, deleted once it's run", "quicker to write"))
assert V.verdict(edited, head, "rival")[0::2] == ("none", 1)
assert V.verdict(dict(pr, body=pr["body"] + "  \n"), head, "rival")[0] == "findings"  # what it was shown, quoted, is the same
unsaid = dict(mine, body=posted["body"].replace(f'"description":"{shown}"', '"description":null'))
assert V.verdict(dict(pr, reviews={"nodes": [unsaid]}), head, "rival")[0::2] == ("none", 1)
github()
with C.acting_for("sam/r"):
    assert V.post("sam/r", pr, "codex", {"findings": []}, shown) == "clean"
    assert "**Codex review** of aaaaaaa: nothing found." in calls()[1][2]["body"]
    # An answer without a list of findings is no review, never a clean
    # one, and nothing is asked of GitHub for it.
    github()
    for bad in ({"summary": "couldn't finish"}, ["not", "a", "dict"]):
        try:
            V.post("sam/r", pr, "rival", bad, shown)
            raise AssertionError(f"{bad} passed as a review")
        except ValueError as e:
            assert "not a review" in str(e), e
    assert calls() == []
    # One GitHub won't take is a different failure (the same answer goes
    # next pass), and so is GitHub not answering, its rate limit noted.
    github(reviews=(422, {"message": "Validation Failed"}))
    try:
        V.post("sam/r", pr, "rival", answer, shown)
        raise AssertionError("a review GitHub didn't take passed")
    except RuntimeError as e:
        assert "didn't take" in str(e) and not isinstance(e, C.Unavailable), e
    github(compare=(403, {"message": "API rate limit exceeded for installation ID 1."}))
    C.LIMITED.clear()
    try:
        V.post("sam/r", pr, "rival", answer, shown)
        raise AssertionError("posted with GitHub unavailable")
    except C.Unavailable as e:
        assert "rate limit" in str(e), e
    assert C.LIMITED == [0.0] and [p for _, p, _, _ in calls()] == [f"repos/sam/r/compare/main...{head}", "rate_limit"], calls()
    C.LIMITED.clear()
# The word posts what it's given: a status run has nothing to post, and a
# reviewer's name goes into its marks, so it's a plain word.
github()
for args, name in ((["--dry"], "rival"), ([], "rival-x"), ([], "")):
    look = {"number": 7, "head": head, "base": "main", "reviewer": name, "name": "R", "answer": {"findings": []}, "description": None}
    p = subprocess.run([*C.CFG["upkeep"], "post", "sam/r", "--now", "1", *args], input=json.dumps(look), capture_output=True, text=True)
    assert p.returncode == 2 and calls() == [], (args, name, p.returncode, p.stderr)
os.environ["PATH"] = kept_path
C.CFG["upkeep"] = kept_upkeep

# A gh call allowed to fail still says why in the journal.
fake = os.path.join(tmp, "bin")
os.makedirs(fake)
with open(f"{fake}/gh", "w") as f:
    f.write("#!/bin/sh\necho \"'needs-owner' not found\" >&2\nexit 1\n")
os.chmod(f"{fake}/gh", 0o755)
path, os.environ["PATH"] = os.environ["PATH"], fake + os.pathsep + os.environ["PATH"]
# Outside acting_for, a call to GitHub doesn't go at all: it would go as the
# machine user's own login. Inside, gh gets the App's token for that owner.
try:
    REAL_GH("api", "repos/sam/r")
    raise AssertionError("a call outside acting_for went out")
except RuntimeError as e:
    assert "outside acting_for" in str(e), e
with open(f"{fake}/gh", "w") as f:
    f.write(f"#!/bin/sh\necho \"$GH_TOKEN\" >{tmp}/gh-token\n")
with C.acting_for("sam/r"):
    REAL_GH("api", "repos/sam/r")
assert open(f"{tmp}/gh-token").read() == "ghs_app_sam\n"
with open(f"{fake}/gh", "w") as f:
    f.write("#!/bin/sh\necho \"'needs-owner' not found\" >&2\nexit 1\n")
with C.acting_for("sam/r"):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        assert REAL_GH("pr", "edit", "5", "--add-label", "needs-owner", check=False) is None
    assert "gh pr edit 5: 'needs-owner' not found" in out.getvalue(), out.getvalue()
    # GitHub not answering is told apart from GitHub saying no.
    for said, unavailable in (("gh: API rate limit exceeded for user ID 1. (HTTP 403)", True),
                              ("gh: Server Error (HTTP 502)", True),
                              ('Post "https://api.github.com/graphql": unexpected EOF', True),
                              ("dial tcp: connect: network is unreachable", True),
                              ("gh: Validation Failed (HTTP 422)", False)):
        with open(f"{fake}/gh", "w") as f:
            f.write(f"#!/bin/sh\necho '{said}' >&2\nexit 1\n")
        try:
            REAL_GH("api", "repos/sam/r/compare/main...x")
            raise AssertionError(f"no error on {said!r}")
        except RuntimeError as e:
            assert isinstance(e, C.Unavailable) == unavailable and said in str(e), (said, e)
    C.gh = REAL_GH
os.environ["PATH"] = path

print("post: ok")
