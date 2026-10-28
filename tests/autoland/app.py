"""The steward as its GitHub App: the tokens it mints from the App's key,
and what counts as the App's own word (its status, its dashboard's author)."""

import base64
import http.server
import json
import os
import subprocess
import sys
import threading
import time

from harness import FAIL, REAL_TOKEN, WROTE, C, P, R, S, V, clean, fresh_pr_state, merges, mkpr, run, sh, stand_in, tmp
from harness import head as HEAD

import dashboard as D  # noqa: E402 (harness puts the steward on the path)

# The App's key, as systemd hands it to a pass, and its public half to check
# what was signed with it.
KEY, PUB = f"{tmp}/cred/app-key", f"{tmp}/app-key.pub"
sh("openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", KEY)
sh("openssl", "pkey", "-in", KEY, "-pubout", "-out", PUB)

# GitHub's API, standing in: the App's installations (sam spelled as GitHub
# has it), and a token for each, numbered.
SEEN, MINTED = [], []
API = {"down": False, "page2": True}


class GitHub(http.server.BaseHTTPRequestHandler):
    def answer(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        SEEN.append(("GET", self.path, self.headers.get("Authorization")))
        if API["down"]:
            return self.answer(503, {"message": "down"})
        # A hundred to a page: sam's on the first, among others' (the App is
        # public), and eve's on the second.
        if self.path == "/app/installations?per_page=100&page=1":
            return self.answer(200, [{"id": 11, "account": {"login": "Sam"}}]
                               + [{"id": 100 + k, "account": {"login": f"other{k}"}} for k in range(99)])
        if self.path == "/app/installations?per_page=100&page=2":
            if not API["page2"]:
                return self.answer(502, {"message": "Bad Gateway"})
            return self.answer(200, [{"id": 12, "account": {"login": "eve"}}])
        self.answer(404, {"message": "Not Found"})

    def do_POST(self):
        SEEN.append(("POST", self.path, self.headers.get("Authorization")))
        if self.path in ("/app/installations/11/access_tokens", "/app/installations/12/access_tokens"):
            MINTED.append(self.path)
            at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 3600))
            return self.answer(201, {"token": f"ghs_minted{len(MINTED)}", "expires_at": at})
        self.answer(404, {"message": "Not Found"})

    def log_message(self, *a):
        pass


api = http.server.HTTPServer(("127.0.0.1", 0), GitHub)
threading.Thread(target=api.serve_forever, daemon=True).start()
C.API = f"http://127.0.0.1:{api.server_port}"


def unb64(s):
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


# A token for sam's repos: the installation is looked up as the App, with a
# JWT its key signed, saying it's App 7, for at most ten minutes.
assert REAL_TOKEN("sam") == "ghs_minted1"
assert MINTED == ["/app/installations/11/access_tokens"], MINTED
# Every page of the installations was read, not just the first.
assert [p for _, p, _ in SEEN] == ["/app/installations?per_page=100&page=1", "/app/installations?per_page=100&page=2",
                                   "/app/installations/11/access_tokens"], SEEN
for method, path, auth in SEEN:
    assert auth.startswith("Bearer "), SEEN
    jwt = auth[len("Bearer "):]
    head, body, sig = jwt.split(".")
    assert json.loads(unb64(head)) == {"alg": "RS256", "typ": "JWT"}
    claims = json.loads(unb64(body))
    assert claims["iss"] == "7" and claims["iat"] <= time.time() and claims["exp"] - claims["iat"] <= 600, claims
    with open(f"{tmp}/jwt.sig", "wb") as f:
        f.write(unb64(sig))
    subprocess.run(["openssl", "dgst", "-sha256", "-verify", PUB, "-signature", f"{tmp}/jwt.sig"],
                   input=f"{head}.{body}".encode(), check=True, capture_output=True)
# Kept for the process: the next call for sam mints nothing; eve's is another
# installation's, and the installations aren't looked up again.
SEEN.clear()
assert REAL_TOKEN("sam") == "ghs_minted1" and SEEN == [], SEEN
assert REAL_TOKEN("eve") == "ghs_minted2" and [p for _, p, _ in SEEN] == ["/app/installations/12/access_tokens"], SEEN
# One with under ten minutes left is minted again.
C._tokens["sam"] = ("ghs_minted1", time.time() + 300)
assert REAL_TOKEN("sam") == "ghs_minted3"
# Never written anywhere: the state dir is the machine user's.
for root, _, files in os.walk(f"{tmp}/state"):
    for name in files:
        with open(os.path.join(root, name), "rb") as f:
            assert b"ghs_minted" not in f.read(), os.path.join(root, name)
# An owner the App isn't installed on gets no token; GitHub not answering
# is Unavailable, as gh's errors are.
try:
    REAL_TOKEN("mallory")
    raise AssertionError("a token for an owner the App isn't on")
except RuntimeError as e:
    assert "isn't installed on mallory" in str(e) and not isinstance(e, C.Unavailable), e
API["down"] = True
try:
    REAL_TOKEN("nobody")
    raise AssertionError("a token with GitHub down")
except C.Unavailable as e:
    assert "HTTP 503" in str(e), e
API["down"] = False
# Nor a page of them: an owner that might be on it isn't said to be missing.
API["page2"] = False
C._installs.clear()
C._tokens.clear()
try:
    REAL_TOKEN("eve")
    raise AssertionError("a token with a page of installations unread")
except C.Unavailable as e:
    assert "HTTP 502" in str(e), e
API["page2"] = True
# Without the key (a pass not given the credential), nothing is signed.
os.rename(KEY, KEY + ".gone")
try:
    C._tokens.clear()
    REAL_TOKEN("sam")
    raise AssertionError("a token without the App's key")
except RuntimeError as e:
    assert "couldn't sign as the App" in str(e), e
os.rename(KEY + ".gone", KEY)
C._tokens.clear()
C._installs.clear()

# git to GitHub (a branch deleted) goes with gh as its only credential
# helper, and the App's token for that repo's owner in its environment.
RAN = []
run_kept = R.subprocess.run
R.subprocess.run = lambda args, **k: RAN.append((args, k.get("env"))) or subprocess.CompletedProcess(args, 0, "", "")
R.delete_branch("sam/r", "old", "1" * 40)
R.subprocess.run = run_kept
push, env = next((a, e) for a, e in RAN if "push" in a)
assert push[push.index("push") - 4:push.index("push")] == list(C.GIT_AUTH), push
assert env["GH_TOKEN"] == "ghs_app_sam", env.get("GH_TOKEN")

# The steward's status is the one its App posted. Anyone who can push can
# post the context, a user whose login is the App's name too (GraphQL gives
# the App's bot that login, without "[bot]"): theirs isn't its word, and a
# pass posts its own again.
def rollup(*creators):
    pr = mkpr()
    pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"] = [
        {"__typename": "StatusContext", "context": "review-settled", "state": "SUCCESS", "description": "settled",
         "creator": c} for c in creators]
    return pr


assert P.our_status(rollup({"login": "steward", "__typename": "Bot"})) == ("success", "settled")
for forged in ({"login": "sam", "__typename": "User"}, {"login": "steward", "__typename": "User"}, None):
    assert P.our_status(rollup(forged)) == (None, ""), forged

# Renovate's dashboard is one an author opened (Renovate runs as the
# machine user's login): a search hit anyone else opened isn't it.
D.FOUND.clear()
gh_kept = C.gh
C.gh = lambda *a, **k: [{"number": 3, "title": "Dependency Dashboard", "user": "mallory"},
                        {"number": 4, "title": "Dependency Dashboard", "user": "sam"}]
st = {}
assert D.find("sam/r", st) == 4, st
D.FOUND.clear()
C.gh = lambda *a, **k: [{"number": 3, "title": "Dependency Dashboard", "user": "mallory"}]
st = {}
assert D.find("sam/r", st) is None and st.get("next"), st
C.gh = gh_kept
D.FOUND.clear()
# The repos are the ones the App's installations reach, asked as each
# owner's: GitHub gives an installation no viewerPermission on any of them.
# An empty repo (no default branch) and an excluded one aren't the steward's.
ASKED = []


def list_gh(*a, **k):
    ASKED.append((C._for[-1] if C._for else None, a[2]))
    return {"sam": [{"nameWithOwner": "sam/r", "isPrivate": True, "defaultBranchRef": {"name": "trunk"}},
                    {"nameWithOwner": "sam/empty", "isPrivate": False, "defaultBranchRef": None},
                    {"nameWithOwner": "sam/skip", "isPrivate": False, "defaultBranchRef": {"name": "main"}}],
            "eve": [{"nameWithOwner": "eve/p", "isPrivate": False, "defaultBranchRef": {"name": "main"}}]}[a[2]]


kept = (C.gh, C.CFG["owners"], C.CFG["exclude"])
C.gh, C.CFG["owners"], C.CFG["exclude"] = list_gh, ["sam", "eve"], ["sam/skip"]
found = R.discover()
C.gh, C.CFG["owners"], C.CFG["exclude"] = kept
assert found == [{"repo": "sam/r", "private": True, "default": "trunk"},
                 {"repo": "eve/p", "private": False, "default": "main"}], found
assert ASKED == [("sam", "sam"), ("eve", "eve")], ASKED

# A pass asks about each repo as the App's installation on that repo's
# owner: its PRs are read for sam/r as sam's, for eve/p as eve's.
FOR = []


def pass_gh(*a, **k):
    FOR.append((C._for[-1] if C._for else None, next(x for x in a if x.startswith("owner="))[len("owner="):]))
    return {"data": {"repository": {"squashMergeAllowed": True, "mergeCommitAllowed": False, "rebaseMergeAllowed": False,
                                    "defaultBranchRef": None,
                                    "pullRequests": {"pageInfo": {"hasNextPage": False}, "nodes": []}}}}


kept = (S.R.discover, S.R.publish, S.R.unhook_gone, S.R.hook_projects, C.gh, sys.argv)
S.R.discover = lambda: [{"repo": "sam/r", "default": "main", "private": False},
                        {"repo": "eve/p", "default": "main", "private": False}]
S.R.publish = S.R.unhook_gone = lambda *a: None
S.R.hook_projects = lambda repos: []
C.gh = pass_gh
sys.argv = ["steward", "kick", "sam/r", "eve/p"]
S.main()
(S.R.discover, S.R.publish, S.R.unhook_gone, S.R.hook_projects, C.gh, sys.argv) = kept
assert FOR == [("sam", "sam"), ("eve", "eve")], FOR

# The steward merges now, the checked head only, or not at all: nothing is
# left for GitHub to merge later (auto-merge, a merge queue), which would
# merge whatever head the base's rules let through. A merge queued before,
# by anyone, is taken off first, and a PR settled and queued by a steward
# before this one is merged now rather than left queued.
stood = (C.gh, C.write, C.job, C.job_state, V.queue, V.post)
stand_in()
fresh_pr_state()
run(clean(), 600)
assert merges() == [f"api -X PUT repos/sam/r/pulls/5/merge -f merge_method=squash -f sha={HEAD}"], WROTE
assert not any(x.startswith("pr merge") for x in WROTE), WROTE
queued_pr = {**clean(), "autoMergeRequest": {"enabledAt": "x"}}
run(queued_pr, 700)
assert WROTE[-2:] == ["pr merge 5 -R sam/r --disable-auto", merges()[0]], WROTE
# The merge queued that couldn't be taken off: no merge either, and again soon.
fresh_pr_state()
FAIL.append("--disable-auto")
run(queued_pr, 800)
FAIL.remove("--disable-auto")
assert merges() == [] and "pr merge 5 -R sam/r --disable-auto" in WROTE, WROTE
(C.gh, C.write, C.job, C.job_state, V.queue, V.post) = stood
fresh_pr_state()
print("app: ok")
