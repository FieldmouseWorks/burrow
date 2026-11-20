"""What renovate-sweep (crates/burrow-renovate) leaves to Python, by its
command line: lock-wait (../modules/autoland/lock_wait.py, the steward's
too), which says when a lock file refresh lock-age refused for age only can
go again. The sweep's own tests run a stand-in for it.

    python3 -I tests/renovate-sweep.py modules/autoland/lock_wait.py
"""

import calendar
import importlib.util
import json
import subprocess
import sys
import time


def wait(login, comments):
    """lock-wait on Renovate's comments on a PR, as REST pages them."""
    p = subprocess.run([sys.executable, "-I", sys.argv[1], login], input=json.dumps([comments]),
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
p = subprocess.run([sys.executable, "-I", sys.argv[1], "--limits"], capture_output=True, text=True, check=True)
assert p.stdout == "3 21600 3 600 3600\n", p.stdout

# The watchdog uses the same wake bound exported to the sweep, with the
# event queue's spacing and the Renovate run ahead of it accounted for.
spec = importlib.util.spec_from_file_location("lock_wait", sys.argv[1])
lock_wait = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lock_wait)
assert lock_wait.interval(7200) == int(p.stdout.split()[-1]) + 20 + 7200
assert lock_wait.recovery(7200) == 3 * (21600 + lock_wait.interval(7200)) + lock_wait.drain(7200)

print("renovate-sweep: ok")
