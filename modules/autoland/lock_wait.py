"""A Renovate lock file refresh that lock-age (../updates/lock-age.py)
refused only because nothing in it is old enough yet. Renovate shows that
as its artifacts status, red, and its comment on the failure carries
lock-age's output, whose last line (lock-age's WAITING) says when the first
of the refresh's releases will be old enough. That's no red CI for a job to
fix but a date: the autoland steward waits for it (step.py), and
renovate-sweep then has Renovate refresh the branch again, which it won't by
itself while the branch doesn't conflict.

    lock-wait LOGIN <gh api --paginate --slurp repos/OWNER/REPO/issues/N/comments

prints when (seconds since the epoch), for renovate-sweep: LOGIN is who
Renovate comments as, as REST spells it: an App's bot as SLUG[bot], a
name no person's can be. Nothing when the PR isn't such a refresh."""

import calendar
import json
import re
import sys
import time

ARTIFACTS = "renovate/artifacts"  # the status Renovate fails a refresh's artifacts with
PROBLEM = "Artifact update problem"  # the title of Renovate's comment on it
WAITING = re.compile(r"lock-age: nothing in the refresh is old enough until (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)")
BLOCK = re.compile(r"^```\n(.*?)^```", re.S | re.M)


def when(body):
    """When the refresh Renovate's comment is about has something old
    enough, or None: only when every failure in it is lock-age's for age."""
    if PROBLEM not in body:
        return None
    times = []
    for block in BLOCK.findall(body):
        lines = [x for x in block.splitlines() if x.strip()]
        m = WAITING.fullmatch(lines[-1].strip()) if lines else None
        if not m:
            return None
        times.append(calendar.timegm(time.strptime(m.group(1), "%Y-%m-%dT%H:%M:%SZ")))
    return max(times, default=None)


def ripe(comments, renovate):
    """when() for the newest of Renovate's comments on the failure (oldest
    first, as GitHub gives them): it keeps one, and takes it down once the
    artifacts go through. renovate(author) tells its comments."""
    body = None
    for c in comments:
        if renovate(c.get("author")) and PROBLEM in (c.get("body") or ""):
            body = c["body"]
    return when(body) if body else None


if __name__ == "__main__":
    login = sys.argv[1]
    comments = [{"author": c.get("user"), "body": c.get("body")} for page in json.load(sys.stdin) for c in page]
    got = ripe(comments, lambda a: bool(login) and (a or {}).get("login") == login)
    if got is not None:
        print(got)
