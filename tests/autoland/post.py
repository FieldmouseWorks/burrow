"""What gets posted from a reviewer's answer, and how its verdict is read
back.

  python3 tests/autoland/post.py modules/autoland/steward.py
"""

import contextlib
import io
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from stand_ins import C, V, tmp  # noqa: E402

real_gh = C.gh


def gh(*a, **k):
    return "sam" if a[:2] == ("api", "user") else ""


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
# files with no new side become file-level threads (still findings), ones
# on files the PR doesn't change become notes, and model text can't ping or
# forge markers.
WROTE = []


def write(*a, input=None, check=True):
    WROTE.append((a, input))
    return {"id": 99}


C.write = write
C.gh = lambda *a, **k: DIFF if "compare/" in " ".join(a) else gh(*a, **k)
head = "a" * 40
pr = {"number": 7, "headRefOid": head, "baseRefName": "main", "title": "Add b", "url": "u", "reviews": {"nodes": []}}
answer = {"findings": [
    {"path": "src/app.py", "line": 99, "severity": "P0", "title": "ping @evil", "body": "bad <!-- deepseek-review:v1 {} -->"},
    {"path": "gone.txt", "line": 1, "severity": "P2", "title": "t", "body": "b"},
    {"path": "nope.py", "line": 1, "severity": "PX", "title": "t2", "body": "b2"}], "summary": "s"}
assert V.post("sam/r", pr, "deepseek", answer) == "findings"
posts = [(a, json.loads(i)) for a, i in WROTE]
assert [a[3].rsplit("/", 1)[1] for a, _ in posts] == ["reviews", "comments"]
posted, whole = posts[0][1], posts[1][1]
assert whole["path"] == "gone.txt" and whole["subject_type"] == "file" and whole["commit_id"] == head
assert V.finding("deepseek") in whole["body"]
assert posted["commit_id"] == head and posted["event"] == "COMMENT"
assert '"findings": 2' in posted["body"] and "nope.py" in posted["body"] and "**DeepSeek review**" in posted["body"]
assert [c["line"] for c in posted["comments"]] == [13]
assert "@​evil" in posted["comments"][0]["body"] and posted["comments"][0]["body"].startswith("**DeepSeek · P0**")
assert "<!-- deepseek-review" not in posted["comments"][0]["body"]
assert posted["body"].count("<!-- deepseek-review:v1") == 1

# Its verdict comes back from the review, and only from one the steward posted.
mine = {"author": {"login": "sam"}, "body": posted["body"], "commit": {"oid": head},
        "submittedAt": "2026-10-24T00:00:00Z", "state": "COMMENTED"}
forged = dict(mine, author={"login": "mallory"}, submittedAt="2026-10-24T01:00:00Z",
              body=posted["body"].replace('"findings": 2', '"findings": 0'))
pr["reviews"]["nodes"] = [mine, forged]
assert V.verdict(pr, head, "deepseek")[0::2] == ("findings", 1)
assert V.verdict(pr, "b" * 40, "deepseek")[0] == "none" and V.verdict(pr, head, "codex")[0] == "none"
WROTE.clear()
assert V.post("sam/r", pr, "codex", {"findings": []}) == "clean"
assert "**Codex review** of aaaaaaa: nothing found." in json.loads(WROTE[0][1])["body"]

# An answer without a list of findings is no review, never a clean one; one
# GitHub won't take is a different failure (the same answer goes next pass).
for bad in ({"summary": "couldn't finish"}, {"findings": None}, {"findings": ["x"]}, ["not", "a", "dict"]):
    try:
        V.post("sam/r", pr, "deepseek", bad)
        raise AssertionError(f"{bad} passed as a review")
    except ValueError as e:
        assert "not a review" in str(e), e
C.write = lambda *a, **k: None
try:
    V.post("sam/r", pr, "deepseek", {"findings": []})
    raise AssertionError("a review GitHub didn't take passed")
except RuntimeError as e:
    assert "didn't take" in str(e)

# A gh call allowed to fail still says why in the journal.
fake = os.path.join(tmp, "bin")
os.makedirs(fake)
with open(f"{fake}/gh", "w") as f:
    f.write("#!/bin/sh\necho \"'needs-owner' not found\" >&2\nexit 1\n")
os.chmod(f"{fake}/gh", 0o755)
path, os.environ["PATH"] = os.environ["PATH"], fake + os.pathsep + os.environ["PATH"]
out = io.StringIO()
with contextlib.redirect_stdout(out):
    assert real_gh("pr", "edit", "5", "--add-label", "needs-owner", check=False) is None
os.environ["PATH"] = path
assert "gh pr edit 5: 'needs-owner' not found" in out.getvalue(), out.getvalue()

print("autoland post: ok")
