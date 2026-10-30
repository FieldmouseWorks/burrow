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
name no person's can be. Nothing when the PR isn't such a refresh.

    lock-wait --limits

prints how far renovate-sweep goes asking for that refresh and taking a
request back: ASKS STANDING RELEASES RELEASE_GAP. The steward counts the same
limits into the watchdog's deadline (recovery, drain), so no stall reaches a
job while the sweep is still at it."""

import calendar
import json
import re
import sys
import time

ARTIFACTS = "renovate/artifacts"  # the status Renovate fails a refresh's artifacts with
PROBLEM = "Artifact update problem"  # the title of Renovate's comment on it
WAITING = re.compile(r"lock-age: nothing in the refresh is old enough until (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)")
BLOCK = re.compile(r"^```\n(.*?)^```", re.S | re.M)
REQUEST = "rebase!"  # the title's prefix that has Renovate redo the branch, on its every run
ASKS, STANDING = 3, 6 * 3600  # requests on a head, and how long each may stand
RELEASES, RELEASE_GAP = 3, 600  # tries to take one back, and how far apart
PASS = 3600  # the longest between the sweep's passes (a Renovate run's, hourly)


def drain(run):
    """The longest from renovate-sweep seeing a request to its job being let
    at the branch: taking it back, then the longest Renovate run (`run`
    seconds), which may have read it just before."""
    return PASS + RELEASES * RELEASE_GAP + run


def recovery(run):
    """The longest from the date to the sweep handing the PR to a job, each
    request standing as long as it may."""
    return ASKS * (STANDING + PASS) + drain(run)


def guard(run):
    """What a job the watchdog hands the PR to is told, as the sweep's is."""
    return (f"; if its title starts with {REQUEST}, which has Renovate redo the branch over what you push, take that "
            f"off the title first, and push nothing to it for {run // 60} minutes after")


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


def refresh(pr, red, names, renovate):
    """What a Renovate PR's wait counts (waits.note), when the sweep may have
    a refresh of it going on: it's red, or a request stands."""
    if not red and not pr["title"].startswith(REQUEST):
        return {}
    return {"refreshing": True, "ripe": ripe(pr["comments"]["nodes"], renovate) if red and names == [ARTIFACTS] else None}


if __name__ == "__main__":
    if sys.argv[1] == "--limits":
        print(ASKS, STANDING, RELEASES, RELEASE_GAP)
        sys.exit()
    login = sys.argv[1]
    comments = [{"author": c.get("user"), "body": c.get("body")} for page in json.load(sys.stdin) for c in page]
    got = ripe(comments, lambda a: bool(login) and (a or {}).get("login") == login)
    if got is not None:
        print(got)
