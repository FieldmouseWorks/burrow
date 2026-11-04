"""What renovate-sweep (crates/burrow-renovate) leaves to Python, by their
command lines: major-seal, which seals a job's verdict on a major with the
token its task gives it and checks a seal (with the seal the autoland
steward checks it with), and lock-wait (../modules/autoland/lock_wait.py,
the steward's too), which says when a lock file refresh lock-age refused
for age only can go again. The sweep's own tests run stand-ins for both.

    python3 -I tests/renovate-sweep.py modules/updates/major-seal.py \
      modules/autoland/seal.py modules/autoland/lock_wait.py
"""

import calendar
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

REPO = "sam/r"
tmp = tempfile.mkdtemp()
# major-seal and the seal beside it, as ../modules/updates.nix builds them.
sealed = os.path.join(tmp, "seal")
os.makedirs(sealed)
shutil.copy(sys.argv[1], os.path.join(sealed, "major-seal.py"))
shutil.copy(sys.argv[2], os.path.join(sealed, "seal.py"))
spec = importlib.util.spec_from_file_location("seal", sys.argv[2])
S = importlib.util.module_from_spec(spec)
spec.loader.exec_module(S)
KEY = "ab" * 32
TOKEN = "0123456789abcdef" * 2


def state(key=KEY):
    """A sweep's state dir: its key, and the token of a job handed PRs 1
    and 3 of REPO as majors."""
    d = os.path.join(tempfile.mkdtemp(dir=tmp), "renovate-sweep")
    os.makedirs(os.path.join(d, "seal"))
    if key:
        with open(os.path.join(d, "verdict.key"), "w") as f:
            f.write(key)
    with open(os.path.join(d, "seal", TOKEN), "w") as f:
        f.write(f"{REPO}\n1\n3\n")
    return d


def seal(st, *a):
    """major-seal on state dir `st`: (exit status, what it printed)."""
    p = subprocess.run([sys.executable, "-E", "-s", os.path.join(sealed, "major-seal.py"), st, *a],
                       capture_output=True, text=True)
    return p.returncode, p.stdout.strip()


def sha(n):
    return f"{n:040x}"


# The job seals its verdict with the token its task gives it, for a major
# that task handed it: what it prints ends its comment, and the seal in it
# is the one the steward checks.
st = state()
code, line = seal(st, TOKEN, "1", "ok", sha(1), "main")
assert code == 0 and line.startswith(f"<!-- major-verdict: ok {sha(1)} on main ") and line.endswith(" -->"), (code, line)
mac = line.split()[-2]
assert mac == S.major(KEY, REPO, 1, "ok", sha(1), "main"), mac
assert seal(st, TOKEN, "3", "held", sha(3), "release")[1].split()[-2] == S.major(KEY, REPO, 3, "held", sha(3), "release")
# --check takes that seal, the repo in any case (a clone's remote needn't
# spell it as GitHub does), and nothing else: another verdict, head, base,
# PR or repo, another key's, or one with no key.
assert seal(st, "--check", REPO, "1", "ok", sha(1), "main", mac) == (0, "")
assert seal(st, "--check", "Sam/R", "1", "ok", sha(1), "main", mac) == (0, "")
for a in ((REPO, "1", "held", sha(1), "main"), (REPO, "1", "ok", sha(2), "main"), (REPO, "1", "ok", sha(1), "release"),
          (REPO, "3", "ok", sha(1), "main"), ("sam/q", "1", "ok", sha(1), "main")):
    assert seal(st, "--check", *a, mac) == (1, ""), a
for other in (state("cd" * 32), state(None)):
    assert seal(other, "--check", REPO, "1", "ok", sha(1), "main", mac) == (1, ""), other
# It seals only for a PR the token's task handed over as a major, with that
# token, and only a verdict it can read back.
for a in ((TOKEN, "6", "ok", sha(6), "main"), ("0" * 32, "1", "ok", sha(1), "main"),
          ("../" + TOKEN, "1", "ok", sha(1), "main"), (TOKEN, "1", "maybe", sha(1), "main"),
          (TOKEN, "1", "ok", "abc", "main"), (TOKEN, "1", "ok", sha(1), "a>b"), (TOKEN, "1", "ok", sha(1))):
    assert seal(st, *a) == (2, ""), a


def wait(login, comments):
    """lock-wait on Renovate's comments on a PR, as REST pages them."""
    p = subprocess.run([sys.executable, "-I", sys.argv[3], login], input=json.dumps([comments]),
                       capture_output=True, text=True)
    assert p.returncode == 0, p.stderr
    return p.stdout.strip()


def refusal(ripe, also=""):
    at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ripe))
    return ("### ⚠️ Artifact update problem\n\nRenovate failed to update an artifact related to this branch.\n\n"
            "##### File name: undefined\n\n```\nCommand failed: /nix/store/x-lock-age/bin/lock-age main\n"
            "lock-age: Cargo.lock: c 0.4.1 is too new, took 0.4.0\nlock-age: Cargo.lock: nothing in the refresh was old "
            "enough, so it went back to the base's, and Renovate would commit its own instead\n"
            f"lock-age: nothing in the refresh is old enough until {at}\n{also}\n```\n\n")


ripe = calendar.timegm(time.gmtime(time.time() + 3 * 86400))


def said(by, body):
    return {"user": {"login": by}, "body": body}


# lock-age's refusal for age, from whom Renovate comments as: when.
assert wait("me", [said("me", refusal(ripe))]) == str(ripe)
assert wait("sam-workers[bot]", [said("sam-workers[bot]", refusal(ripe))]) == str(ripe)
# The newest of Renovate's comments on it is the one.
assert wait("me", [said("me", refusal(ripe - 60)), said("me", refusal(ripe))]) == str(ripe)
# Nothing for anyone else's comment (a user who holds an App's bot's name
# too), a failure that isn't only age, none at all, or no login to ask as.
for login, comments in (("me", [said("mallory", refusal(ripe))]), ("sam-workers[bot]", [said("sam-workers", refusal(ripe))]),
                        ("me", [said("me", refusal(ripe, "error: failed to select a version"))]), ("me", []),
                        ("", [said("", refusal(ripe))])):
    assert wait(login, comments) == "", (login, comments)
# How far the sweep asks and takes a request back, which the steward
# counts into its deadlines too.
p = subprocess.run([sys.executable, "-I", sys.argv[3], "--limits"], capture_output=True, text=True, check=True)
assert p.stdout == "3 21600 3 600\n", p.stdout

print("renovate-sweep: ok")
