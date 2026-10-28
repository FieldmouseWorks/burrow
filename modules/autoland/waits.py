"""What the steward waits on, for burrow-watchdog (../watchdog.nix): each
open PR it saw to, what its status says it waits on (its phase) and since
when, and by when that should be over. Saved after every pass, so a file
that stops changing is a steward that has stopped. Past an item's deadline
the watchdog kicks its pass, then hands it to a Claude job, then to the
stuck issue, which the steward publishes (escalated()).

Not watched: a draft, or a major renovate-sweep held for the owner (nobody
waits on the pipeline there), and a PR this pass listed for the owner or on
the stuck issue (it's theirs, or that issue's job's, already). One on hold
gets holdStallAfter: a hold nobody explains is a stall too, and the job that
takes it moves one waiting on the owner's word to the owner label."""

import json

import common as C
from common import CFG, MIN, load, log, repo_cfg, save

# What a PR's wait is, for its deadline and the pass a stall kicks first:
# the steward's own, a hold (no pass moves it), or one renovate-sweep has
# (its pass asks for the verdict again).
STEWARD, HOLD, SWEEP = "steward", "hold", "sweep"
FAILS = "the steward's pass fails on it (journalctl -u autoland)"

NOTED = {}  # this pass's waits, by repo whose PRs it read, then by key
FAILED = {}  # the repos this pass failed on before it read their PRs


def begin():
    NOTED.clear()
    FAILED.clear()


def repo_of(key):
    return key.partition("#")[0]


def read(repo):
    """The repo's PRs were read: what's noted for it now replaces what it had."""
    NOTED[repo] = {}


def item(repo, pr, phase, minutes, kick):
    # .get: what failed may be the PR's own record.
    return {"what": f"{repo}#{pr['number']} {pr.get('title', '')}".strip(), "link": pr.get("url"), "phase": phase,
            "minutes": minutes, "kick": kick}


def note(repo, pr, phase, wait):
    """The PR's wait as its status says it (its last in a pass stands); wait
    None: nothing to watch."""
    key = f"{repo}#{pr['number']}"
    if wait is None:
        NOTED.setdefault(repo, {}).pop(key, None)
        return
    sweep = wait == SWEEP and CFG["watch"] and CFG["watch"]["renovateSweep"]
    kick = None if wait == HOLD else f"renovate-sweep/{repo}" if sweep else f"autoland/{repo}"
    NOTED.setdefault(repo, {})[key] = item(repo, pr, phase, repo_cfg(repo)["holdStallAfter" if wait == HOLD else "stallAfter"], kick)


def failed(repo, pr=None):
    """The pass failed on the PR, or on the repo before it read its PRs."""
    minutes, kick = repo_cfg(repo)["stallAfter"], f"autoland/{repo}"
    if pr:
        NOTED.setdefault(repo, {})[f"{repo}#{pr['number']}"] = item(repo, pr, FAILS, minutes, kick)
    else:
        FAILED[repo] = {"what": repo, "link": f"https://github.com/{repo}", "phase": FAILS, "minutes": minutes,
                        "kick": kick}


def publish(seen, digest, full):
    """watch.json: this pass's waits for the repos it saw (one whose PRs it
    couldn't read keeps the ones it had), and a kicked pass keeps the other
    repos' as they were. A wait in the phase it had keeps its start; a PR
    listed in this pass's digest isn't one. The watchdog is kicked when they
    changed: what's new may fall due before the pass it booked, and what has
    moved on is no stall."""
    if not CFG["watch"]:
        return
    was = load("watch.json", {}).get("items", {})
    items = {k: v for k, v in was.items()
             if (repo_of(k) not in seen and not full) or (repo_of(k) in seen and repo_of(k) not in NOTED and k != repo_of(k))}
    for repo in seen:
        listed = [x[1] for x in digest.get(repo, [])]
        now = {**NOTED.get(repo, {}), **({repo: FAILED[repo]} if repo in FAILED else {})}
        for key, it in now.items():
            if any(x.startswith(f"[{key}](") for x in listed):
                continue
            old = was.get(key, {})
            since = old["since"] if old.get("phase") == it["phase"] else C.NOW
            items[key] = {"what": it["what"], "link": it["link"], "phase": it["phase"], "since": since,
                          "due": since + it["minutes"] * MIN, "kick": it["kick"]}
    save("watch.json", {"at": C.NOW, "items": items})
    if items != was:
        log(f"waits changed: {len(items)} watched")
        C.poke(CFG["watch"]["kick"])


def escalated():
    """The stalls the watchdog has taken past a Claude job, for the stuck
    issue: (why, line, desc) each."""
    if not CFG["watch"]:
        return []
    try:
        with open(CFG["watch"]["escalated"]) as f:
            got = json.load(f)
    except (OSError, ValueError):
        return []
    return [tuple(x) for x in got if isinstance(x, list) and len(x) == 3 and all(isinstance(s, str) for s in x)] \
        if isinstance(got, list) else []
