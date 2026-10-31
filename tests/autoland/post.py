"""What gets posted from a reviewer's answer, and how its verdict is read
back."""

import contextlib
import io
import json
import os

from harness import REAL_GH, C, V, tmp


def gh(*a, **k):
    return ""


DIFF = """diff --git a/src/app.py b/src/app.py
index 111..222 100644
--- a/src/app.py
+++ b/src/app.py
@@ -10,4 +10,5 @@ def f():
     a = 1
---- a removed line that looks like a header
+    b = 2
+    c = 3
     return a
diff --git "a/cert-\\303\\251.pem" "b/cert-\\303\\251.pem"
new file mode 100644
--- /dev/null
+++ "b/cert-\\303\\251.pem"
@@ -0,0 +1 @@
+x
diff --git a/weird header
@@ -0,0 +1 @@
+x
diff --git a/foo b/.env b/config.txt
similarity index 90%
rename from foo b/.env
rename to config.txt
--- a/foo b/.env\t
+++ b/config.txt
@@ -1,2 +1,2 @@
 x=0
-x=1
+x=2
diff --git a/my notes.txt b/my notes.txt
--- a/my notes.txt\t
+++ b/my notes.txt\t
@@ -1 +1 @@
-old
+new
diff --git a/start.sh b/start.sh
old mode 100755
new mode 100644
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
"""

files = V.commentable(DIFF)
assert files["src/app.py"] == {10, 11, 12, 13}, files
# Git quotes unusual names; they're read, and a header that can't be read is skipped.
assert files["cert-é.pem"] == {1} and len(files) == 6, files
# Git leaves spaces bare, so a header can split more than one way: every
# way is checked, and a plain name with a space is still read.
assert V.header_pairs("diff --git a/foo b/.env b/config.txt") == [("foo", ".env b/config.txt"), ("foo b/.env", "config.txt")]
assert files["config.txt"] == {1, 2} and files["my notes.txt"] == {1}
assert files["start.sh"] == set() and files["gone.txt"] == set()

# One review: findings off a changed line snap to the nearest, findings on
# files with no new side become file-level threads, ones on files the PR
# doesn't change become threads on its first changed file that say where
# (all still findings), and model text can't ping or forge markers.
WROTE = []


def write(*a, input=None, check=True):
    WROTE.append((a, input))
    return {"id": 99}


C.write = write
C.gh = lambda *a, **k: DIFF if "compare/" in " ".join(a) else gh(*a, **k)
head = "a" * 40
pr = {"number": 7, "headRefOid": head, "baseRefName": "main", "title": "Add b", "url": "u", "reviews": {"nodes": []},
      "body": "Adds b.\n\nWhy not Rust: a throwaway script, deleted once it's run"}
shown = C.fingerprint(pr["body"])  # the description the reviews were shown
answer = {"findings": [
    {"path": "src/app.py", "line": 99, "severity": "P0", "title": "ping @evil", "body": "bad <!-- rival-review:v1 {} -->",
     "covers": "every caller, @evil's too"},
    {"path": "gone.txt", "line": 1, "severity": "P2", "title": "t", "body": "b"},
    {"path": "nope.py", "line": 1, "severity": "PX", "title": "t2", "body": "b2"}], "summary": "s"}
assert V.post("sam/r", pr, "rival", answer, shown) == "findings"
posts = [(a, json.loads(i)) for a, i in WROTE]
assert [a[3].rsplit("/", 1)[1] for a, _ in posts] == ["reviews", "comments", "comments"]
posted, whole, elsewhere = posts[0][1], posts[1][1], posts[2][1]
assert whole["path"] == "gone.txt" and whole["subject_type"] == "file" and whole["commit_id"] == head
assert V.finding("rival") in whole["body"]
assert elsewhere["path"] == "src/app.py" and elsewhere["subject_type"] == "file" and V.finding("rival") in elsewhere["body"]
assert "(In `nope.py`, which this PR doesn't change.)" in elsewhere["body"] and "t2" in elsewhere["body"], elsewhere
assert posted["commit_id"] == head and posted["event"] == "COMMENT"
assert '"findings": 3' in posted["body"] and "**Rival review**" in posted["body"]
assert [c["line"] for c in posted["comments"]] == [13]
assert "@​evil" in posted["comments"][0]["body"] and posted["comments"][0]["body"].startswith("**Rival · P0**")
assert "<!-- rival-review" not in posted["comments"][0]["body"]
# What a complete fix covers goes in the thread, for whoever fixes it.
assert "\n\n**A complete fix covers:** every caller, @​evil's too" in posted["comments"][0]["body"], posted["comments"][0]["body"]
assert "complete fix" not in whole["body"], whole["body"]
assert posted["body"].count("<!-- rival-review:v1") == 1 and "no changed file to go on" not in posted["body"]
# File-level ones are in the review's own body in full too, so one GitHub
# won't take as a thread loses nothing, with no second call to fail.
assert "- `gone.txt`: **Rival · P2** t b" in posted["body"], posted["body"]
assert "- `nope.py`: **Rival · P1** t2 b2" in posted["body"] and V.finding("rival") not in posted["body"], posted["body"]
WROTE.clear()
C.write = lambda *a, **k: write(*a, **k) if "/reviews" in a[3] else None
assert V.post("sam/r", pr, "rival", answer, shown) == "findings"
assert [a[2] for a, _ in WROTE] == ["POST"] and "- `gone.txt`: " in json.loads(WROTE[0][1])["body"], WROTE
# One GitHub won't take leaves no file-level threads to be posted again.
WROTE.clear()
C.write = lambda *a, **k: write(*a, **k) if "/comments" in a[3] else None
try:
    V.post("sam/r", pr, "rival", answer, shown)
    raise AssertionError("a review GitHub didn't take passed")
except RuntimeError:
    assert WROTE == [], WROTE
C.write = write

# Its verdict comes back from the review, and only from one the steward's
# App posted: not a user's, even one whose login is the App's name, which
# GraphQL gives the App's bot too, without "[bot]".
mine = {"author": {"login": "steward", "__typename": "Bot"}, "body": posted["body"], "commit": {"oid": head},
        "submittedAt": "2026-10-24T00:00:00Z", "state": "COMMENTED"}
forged = dict(mine, author={"login": "mallory", "__typename": "User"}, submittedAt="2026-10-24T01:00:00Z",
              body=posted["body"].replace('"findings": 2', '"findings": 0'))
namesake = dict(forged, author={"login": "steward", "__typename": "User"})
pr["reviews"]["nodes"] = [mine, forged, namesake]
assert V.verdict(pr, head, "rival")[0::2] == ("findings", 1)
assert V.verdict(pr, "b" * 40, "rival")[0] == "none" and V.verdict(pr, head, "codex")[0] == "none"
# Retargeted, the same head is another change: what was said on main doesn't count.
assert V.verdict(dict(pr, baseRefName="release"), head, "rival")[0] == "none"
# It's a verdict on the description it was shown too: one edited since (a
# reason written in after a clean review, for CI) has had nobody weigh it,
# and one whose review doesn't say what it was shown is no verdict at all.
assert f'"description": "{shown}"' in posted["body"], posted["body"]
edited = dict(pr, body=pr["body"].replace("a throwaway script, deleted once it's run", "quicker to write"))
assert V.verdict(edited, head, "rival")[0::2] == ("none", 1)
assert V.verdict(dict(pr, body=pr["body"] + "  \n"), head, "rival")[0] == "findings"  # what it was shown, quoted, is the same
unsaid = dict(mine, body=posted["body"].replace(f'"description": "{shown}"', '"description": null'))
assert V.verdict(dict(pr, reviews={"nodes": [unsaid]}), head, "rival")[0::2] == ("none", 1)
WROTE.clear()
assert V.post("sam/r", pr, "codex", {"findings": []}, shown) == "clean"
assert "**Codex review** of aaaaaaa: nothing found." in json.loads(WROTE[0][1])["body"]

# An answer without a list of findings is no review, never a clean one; one
# GitHub won't take is a different failure (the same answer goes next pass).
for bad in ({"summary": "couldn't finish"}, {"findings": None}, {"findings": ["x"]}, ["not", "a", "dict"],
            {"findings": [{"path": ["src/app.py"], "line": 1}]}, {"findings": [{"path": "src/app.py", "title": {}}]},
            {"findings": [], "summary": 3}, {"findings": [{"line": 1, "severity": "P1", "title": "t", "body": "b"}]}):
    try:
        V.post("sam/r", pr, "rival", bad, shown)
        raise AssertionError(f"{bad} passed as a review")
    except ValueError as e:
        assert "not a review" in str(e), e
C.write = lambda *a, **k: None
try:
    V.post("sam/r", pr, "rival", {"findings": []}, shown)
    raise AssertionError("a review GitHub didn't take passed")
except RuntimeError as e:
    assert "didn't take" in str(e)
# With no changed file to put one on, a finding is listed in the review,
# and still counts.
C.write = write
WROTE.clear()
C.gh = lambda *a, **k: "diff --git a/weird header\n@@ -0,0 +1 @@\n+x\n" if "compare/" in " ".join(a) else gh(*a, **k)
assert V.post("sam/r", pr, "rival", {"findings": [answer["findings"][2]]}, shown) == "findings"
body = json.loads(WROTE[0][1])["body"]
assert '"findings": 1' in body and "- `nope.py`: **Rival · P1** t2 b2" in body and len(WROTE) == 1, body
# A diff that can't be read (none at all, or not a diff) posts nothing: read
# as one, every finding would be a note and the review clean.
C.write = write
for got in ("", "Not Found"):
    WROTE.clear()
    C.gh = lambda *a, **k: got if "compare/" in " ".join(a) else gh(*a, **k)
    try:
        V.post("sam/r", pr, "rival", answer, shown)
        raise AssertionError(f"posted on a diff of {got!r}")
    except RuntimeError as e:
        assert "couldn't read the diff" in str(e) and WROTE == [], (e, WROTE)

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
    # GitHub not answering is told apart from GitHub saying no, and a review
    # whose diff it couldn't read then says which (steward.py keeps the answer).
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
    with open(f"{fake}/gh", "w") as f:
        f.write("#!/bin/sh\necho 'gh: API rate limit exceeded for user ID 1. (HTTP 403)' >&2\nexit 1\n")
    try:
        V.post("sam/r", pr, "rival", answer, shown)
        raise AssertionError("posted with GitHub unavailable")
    except C.Unavailable as e:
        assert "rate limit" in str(e), e
os.environ["PATH"] = path

print("post: ok")
