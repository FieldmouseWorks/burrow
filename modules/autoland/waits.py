"""What the steward waits on, for burrow-watchdog (../watchdog.nix): each
open PR it saw to, what its status says it waits on (its phase) and since
when, and by when that should be over. Saved after every pass, so a file
that stops changing is a steward that has stopped. Past an item's deadline
the watchdog kicks its pass, then hands it to a Claude job, then to the
stuck issue, which the steward publishes (escalated()).

A wait is the same one while its phase and what it's about (the PR's head
and base, its `rev`) are: a new head saying the same thing is new work, with
a deadline of its own. An open PR a pass noted nothing for (it failed on
the repo first, or left the PR's status as it was) keeps the wait it had; a
repo it fails on is watched itself. One whose step keeps failing the same way
(step.py's strike) is due at once, whatever its deadline, and its phase says
what fails.

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

NOTED = {}  # this pass's waits, by repo whose PRs it read, then by key (None: nothing to watch)
OPEN = {}  # the keys of each repo's open PRs, as this pass read them
FAILED = {}  # the repos this pass failed on, before, between or after their PRs
FAILING = {}  # the PRs whose step keeps failing, by key: what fails


def begin():
    NOTED.clear()
    OPEN.clear()
    FAILED.clear()
    FAILING.clear()


def repo_of(key):
    return key.partition("#")[0]


def key_of(repo, pr):
    return f"{repo}#{pr['number']}"


def read(repo, prs):
    """The repo's open PRs were read: what's noted for them replaces what
    they had, and a closed one's is gone."""
    NOTED[repo] = {}
    OPEN[repo] = {key_of(repo, pr) for pr in prs}


def item(repo, pr, phase, minutes, kick, rev):
    # .get: what failed may be the PR's own record.
    return {"what": f"{repo}#{pr['number']} {pr.get('title', '')}".strip(), "link": pr.get("url"), "phase": phase,
            "minutes": minutes, "kick": kick, "rev": rev}


def note(repo, pr, phase, wait):
    """The PR's wait as its status says it (its last in a pass stands); wait
    None: nothing to watch."""
    if wait is None:
        NOTED.setdefault(repo, {})[key_of(repo, pr)] = None
        return
    sweep = wait == SWEEP and CFG["watch"] and CFG["watch"]["renovateSweep"]
    kick = None if wait == HOLD else f"renovate-sweep/{repo}" if sweep else f"autoland/{repo}"
    minutes = repo_cfg(repo)["holdStallAfter" if wait == HOLD else "stallAfter"]
    rev = f"{pr['headRefOid']} {pr['baseRefName']}"
    NOTED.setdefault(repo, {})[key_of(repo, pr)] = item(repo, pr, phase, minutes, kick, rev)


def overdue(repo, pr, why):
    """The PR's step keeps failing the same way: its wait is due now. False
    without the watchdog, which nothing then tells."""
    if not CFG["watch"]:
        return False
    FAILING[key_of(repo, pr)] = why
    return True


def failed(repo, pr=None):
    """The pass failed on the PR, or on the repo. Whatever its head, it's the
    steward's pass that fails: no rev."""
    minutes, kick = repo_cfg(repo)["stallAfter"], f"autoland/{repo}"
    if pr:
        NOTED.setdefault(repo, {})[key_of(repo, pr)] = item(repo, pr, FAILS, minutes, kick, None)
    else:
        FAILED[repo] = {"what": repo, "link": f"https://github.com/{repo}", "phase": FAILS, "minutes": minutes,
                        "kick": kick, "rev": None}


def publish(seen, digest, full):
    """watch.json: this pass's waits for the repos it saw, and a kicked pass
    keeps the other repos' as they were. An open PR it noted nothing for
    (every one, in a repo whose PRs it couldn't read) keeps the wait it had.
    The same wait keeps its start; a PR listed in this pass's digest isn't
    one. The watchdog is kicked when they changed: what's new may fall due
    before the pass it booked, and what has moved on is no stall."""
    if not CFG["watch"]:
        return
    was = load("watch.json", {}).get("items", {})
    items = {k: v for k, v in was.items() if repo_of(k) not in seen and not full}
    for repo in seen:
        noted = NOTED.get(repo)
        listed = [x[1] for x in digest.get(repo, [])]

        def theirs(key):
            return any(x.startswith(f"[{key}](") for x in listed)

        items.update({k: v for k, v in was.items() if repo_of(k) == repo and k != repo and not theirs(k)
                      and (noted is None or (k in OPEN[repo] and k not in noted))})
        now = {**{k: v for k, v in (noted or {}).items() if v}, **({repo: FAILED[repo]} if repo in FAILED else {})}
        for key, it in now.items():
            if theirs(key):
                continue
            old = was.get(key, {})
            phase = f"{it['phase']}; {FAILING[key]}" if key in FAILING else it["phase"]
            same = old.get("phase") == phase and old.get("rev") == it["rev"]
            since = old["since"] if same else C.NOW
            items[key] = {"what": it["what"], "link": it["link"], "phase": phase, "since": since,
                          "due": since if key in FAILING else since + it["minutes"] * MIN, "kick": it["kick"],
                          "rev": it["rev"]}
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
