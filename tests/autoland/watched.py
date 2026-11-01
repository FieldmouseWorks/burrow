"""What the steward waits on, for the stall watchdog (waits.py): every open
PR's wait as its status says it, since when and by when, saved each pass;
which PRs aren't watched; a new head; a kicked pass; a repo or PR the pass
fails on, before, between or after its PRs; and what the watchdog escalated
going on the stuck issue."""

import json
import os
import sys
import time

from harness import CI_GREEN, KICKS, WROTE, T, C, S, fresh_pr_state, info, mkpr, said, stand_in, tmp

import waits

stand_in()
MIN = 60
published = []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.watch_default, S.branches, S.V.snap_name)
S.R.settle_repo = lambda r, known: False
S.R.publish = published.append
REPOS = ["sam/r", "sam/q"]
S.R.discover = lambda: [{"repo": r, "default": "main"} for r in REPOS]
OPEN = {}  # each repo's open PRs, or None: reading them fails


def pr(repo, n, **kw):
    return {**mkpr(number=n, **{"title": f"PR {n}", **kw}), "url": f"https://github.com/{repo}/pull/{n}"}


def open_prs(owner, name):
    got = OPEN.get(f"{owner}/{name}", [])
    if got is None:
        raise RuntimeError("GitHub said no")
    return info, got


S.open_prs = open_prs


def run(at, *kick):
    """A pass at T + at: a full one, or one kicked for those repos."""
    C.NOW = T + at
    C.WAKE.clear()
    KICKS.clear()
    WROTE.clear()
    published.clear()
    argv, sys.argv = sys.argv, ["steward", "kick", *kick] if kick else ["steward"]
    try:
        S.main()
    finally:
        sys.argv = argv
    got = json.load(open(f"{tmp}/state/watch.json"))
    assert got["at"] == T + at, got
    return got["items"]


def kicked():
    return "/kick/watchdog" in KICKS


fresh_pr_state()
major = {"branch": "renovate/foo-2.x", "title": "Update foo from 1.4 to 2.0"}
OPEN["sam/r"] = [pr("sam/r", 5), pr("sam/r", 6, draft=True), pr("sam/r", 7, labels=["hold"]), pr("sam/r", 8, **major)]
OPEN["sam/q"] = [pr("sam/q", 1, labels=["needs-owner"])]
items = run(100)
# A draft isn't watched, nor is one listed for the owner (or on the stuck
# issue): nobody waits on the pipeline there, or that issue has it already.
assert sorted(items) == ["sam/r#5", "sam/r#7", "sam/r#8"], items
five = items["sam/r#5"]
assert five["what"] == "sam/r#5 PR 5" and five["link"] == "https://github.com/sam/r/pull/5", five
assert five["since"] == T + 100 and five["due"] == T + 100 + 120 * MIN and five["kick"] == "autoland/sam/r", five
# Its wait is what its status says.
assert any(f"description={five['phase']} -f target_url={five['link']}" in x for x in WROTE), (five, WROTE)
# A hold gets its own allowance, and no pass to kick: one waits on a person.
assert items["sam/r#7"]["phase"].startswith("On hold") and items["sam/r#7"]["due"] == T + 100 + 480 * MIN, items
assert items["sam/r#7"]["kick"] is None, items
# What renovate-sweep has: its pass is the one that asks again.
assert "renovate-sweep" in items["sam/r#8"]["phase"] and items["sam/r#8"]["kick"] == "renovate-sweep/sam/r", items
# They're new: the watchdog is kicked to book their deadlines.
assert kicked(), KICKS

# Waiting on the same thing, a wait keeps its start, and the watchdog
# isn't kicked for nothing new.
again = run(200)
assert again["sam/r#5"]["since"] == T + 100 and again["sam/r#5"]["due"] == T + 100 + 120 * MIN, again
assert again == items and not kicked(), (again, KICKS)
# A new head saying the same thing (a Renovate major rebased, still waiting
# for renovate-sweep's verdict on this head) is new work: a new start.
OPEN["sam/r"][3] = pr("sam/r", 8, sha="c" * 40, **major)
rebased = run(250)
assert rebased["sam/r#8"]["phase"] == items["sam/r#8"]["phase"], rebased
assert rebased["sam/r#8"]["since"] == T + 250 and rebased["sam/r#8"]["due"] == T + 250 + 120 * MIN and kicked(), rebased
# A lock file refresh waiting for its releases to be old enough is due its
# allowance after that, not after it began waiting.
ripe = T + 3 * 86400
lock = {"branch": "renovate/cargo-lock-file-maintenance", "title": "Lock file maintenance (cargo)", "checks": [
    *CI_GREEN, {"__typename": "StatusContext", "context": "renovate/artifacts", "state": "FAILURE"}],
    "comments": [said("### Artifact update problem\n\n```\nlock-age: nothing in the refresh is old enough until "
                      f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(ripe))}\n```\n")]}
OPEN["sam/r"].append(pr("sam/r", 9, **lock))
waiting = run(260)
assert "old enough until" in waiting["sam/r#9"]["phase"] and waiting["sam/r#9"]["kick"] == "renovate-sweep/sam/r", waiting
# ...and its allowance counts from after all renovate-sweep can take to get it
# refreshed or hand it to a job itself: every request standing as long as it
# may, one taken back, and the longest Renovate run that read it. Past that
# the job the watchdog hands it to is told to take a request back itself.
# A pass is an hour apart at best, and the hour and a run behind it at worst.
every = 3600 + 7200
drain = 4 * every + 7200
recovery = 3 * (6 * 3600 + every) + drain
assert waiting["sam/r#9"]["since"] == T + 260 and waiting["sam/r#9"]["due"] == ripe + recovery + 120 * MIN, waiting
assert "rebase! at the start of its title" in waiting["sam/r#9"]["phase"] and "until 120 minutes after it last had it" in waiting["sam/r#9"]["phase"], waiting
# A date long gone counts the same from when it began waiting, and the wait
# is the same one whether or not a request stands: asks don't move its deadline.
past = {**lock, "comments": [said("### Artifact update problem\n\n```\nlock-age: nothing in the refresh is old enough until "
                                  f"{time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(T - 3600))}\n```\n")]}
OPEN["sam/r"][-1] = pr("sam/r", 9, **past)
gone = run(262)
assert gone["sam/r#9"]["since"] == T + 262 and gone["sam/r#9"]["due"] == T + 262 + recovery + 120 * MIN, gone
OPEN["sam/r"][-1] = pr("sam/r", 9, **{**past, "title": "rebase!Lock file maintenance (cargo)"})
asking = run(264)
assert asking["sam/r#9"]["since"] == T + 262 and asking["sam/r#9"]["due"] == gone["sam/r#9"]["due"], asking
# Open threads put it down as red CI, which waits for the same date: the sweep
# hands it over no sooner for them.
OPEN["sam/r"][-1] = pr("sam/r", 9, **{**past, "threads": [False]})
threads = run(265)
assert "CI red" in threads["sam/r#9"]["phase"] and threads["sam/r#9"]["due"] == T + 265 + recovery + 120 * MIN, threads
# Any other red Renovate PR, whose request (anyone's) the sweep may be taking
# back: past that, and the longest run that read it, before its allowance.
OPEN["sam/r"][-1] = pr("sam/r", 9, **{**past, "comments": []})
red = run(266)
assert red["sam/r#9"]["due"] == T + 266 + drain + 120 * MIN and "rebase! at the start of its title" in red["sam/r#9"]["phase"], red
# Past that bound's cases: three failed tries an hour apart, the job at hour
# four and its wait to hour six are all inside a default stallAfter's deadline.
assert drain > 6 * 3600, drain
# Green with a request just gone, the job the watchdog hands it to is told the same.
OPEN["sam/r"][-1] = pr("sam/r", 9, **{**past, "comments": [], "checks": CI_GREEN, "title": "rebase!Lock file maintenance (cargo)"})
run(267)
OPEN["sam/r"][-1] = pr("sam/r", 9, **{**past, "comments": [], "checks": CI_GREEN, "renames": [
    {"createdAt": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(T + 267)), "previousTitle": "rebase!Lock file maintenance (cargo)"}]})
just = run(268)
assert "rebase! at the start of its title" in just["sam/r#9"]["phase"], just
OPEN["sam/r"].pop()
run(270)

# On to the next thing (findings answered, CI, a job...): a new start and a
# new deadline, and the watchdog hears.
OPEN["sam/r"][0] = pr("sam/r", 5, threads=[False])
moved = run(300)
assert moved["sam/r#5"]["phase"] != items["sam/r#5"]["phase"], moved
assert moved["sam/r#5"]["since"] == T + 300 and moved["sam/r#5"]["due"] == T + 300 + 120 * MIN and kicked(), moved
# Merged or closed: no longer waited on.
OPEN["sam/r"] = OPEN["sam/r"][1:]
items = run(400)
assert sorted(items) == ["sam/r#7", "sam/r#8"] and kicked(), items
print("waits: ok")

# A kicked pass replaces only its repos' waits; a full one drops a repo the
# steward no longer has.
OPEN["sam/q"] = [pr("sam/q", 2)]
items = run(500, "sam/q")
assert sorted(items) == ["sam/q#2", "sam/r#7", "sam/r#8"], items
OPEN["sam/r"] = []
items = run(600, "sam/r")
assert sorted(items) == ["sam/q#2"], items
REPOS.remove("sam/q")
assert run(700) == {} and kicked()
REPOS.append("sam/q")
print("waits kicked: ok")

# A repo whose PRs it can't read keeps what they waited on, and is watched
# itself: a pass that keeps failing there is the steward not seeing to it.
OPEN["sam/r"] = [pr("sam/r", 5)]
items = run(800)
OPEN["sam/r"] = None
failing = run(900)
assert failing["sam/r#5"] == items["sam/r#5"], failing
assert failing["sam/r"]["phase"] == waits.FAILS and failing["sam/r"]["since"] == T + 900, failing
assert failing["sam/r"]["link"] == "https://github.com/sam/r" and failing["sam/r"]["kick"] == "autoland/sam/r", failing
assert run(1000)["sam/r"]["since"] == T + 900
# Read again, it's what its PRs wait on that's watched.
OPEN["sam/r"] = [pr("sam/r", 5)]
assert sorted(run(1100)) == ["sam/q#2", "sam/r#5"]
# One PR the pass fails on: that PR is, the rest as ever.
OPEN["sam/r"] = [pr("sam/r", 5), {"number": 9, "headRefName": "x", "baseRefName": "main", "headRefOid": "b" * 40}]
items = run(1200)
assert items["sam/r#9"] == {"what": "sam/r#9", "link": None, "phase": waits.FAILS, "since": T + 1200,
                            "due": T + 1200 + 120 * MIN, "kick": "autoland/sam/r", "rev": None}, items
assert items["sam/r#5"]["since"] == T + 1100, items


def broken(*a, **kw):
    raise RuntimeError("GitHub said no")


# Failing on the repo after reading its PRs: before it got to them, they all
# keep the waits they had, and the repo is watched.
OPEN["sam/r"] = [pr("sam/r", 5), pr("sam/r", 7, labels=["hold"])]
items = run(1300)
S.R.watch_default = broken
OPEN["sam/r"][0] = pr("sam/r", 5, threads=[False])  # a new phase, had the pass seen to it
failing = run(1400)
assert failing["sam/r#5"] == items["sam/r#5"] and failing["sam/r#7"] == items["sam/r#7"], failing
assert failing["sam/r"]["phase"] == waits.FAILS and failing["sam/r"]["since"] == T + 1400, failing
S.R.watch_default = kept_fns[4]
# Between them: what it saw to is as it noted, the rest as they were.
S.V.snap_name = lambda name, *a: broken() if name.endswith("-7.json") else kept_fns[6](name, *a)
OPEN["sam/r"][1] = pr("sam/r", 7)  # off hold, had the pass seen to it
failing = run(1500)
assert failing["sam/r#5"]["phase"] != items["sam/r#5"]["phase"] and failing["sam/r#5"]["since"] == T + 1500, failing
assert failing["sam/r#7"] == items["sam/r#7"] and failing["sam/r"]["since"] == T + 1400, failing
S.V.snap_name = kept_fns[6]
# After them: all as noted, and a PR that's closed is gone.
S.branches = broken
OPEN["sam/r"] = OPEN["sam/r"][:1]
failing = run(1600, "sam/r")
assert sorted(failing) == ["sam/q#2", "sam/r", "sam/r#5"] and failing["sam/r#5"]["since"] == T + 1500, failing
assert failing["sam/r"]["since"] == T + 1400, failing
S.branches = kept_fns[5]
assert sorted(run(1700)) == ["sam/q#2", "sam/r#5"]
print("waits failing: ok")

# Open over a day, it's on the stuck issue, which has it from then on.
fresh_pr_state()
OPEN["sam/r"] = [pr("sam/r", 5)]
assert "sam/r#5" in run(100)
assert "sam/r#5" not in run(86400 + 100)
assert any(x[1].startswith("[sam/r#5](") for x in published[0]), published
print("waits listed: ok")

# What the watchdog escalated goes on the stuck issue with the rest, each
# entry once; a broken file is none.
entry = ["Stalled past its deadline, and a Claude job couldn't move it", "[sam/r#5](https://github.com/sam/r/pull/5)",
         "Settled; waiting for CI: build, since 1:00am UTC"]
with open(f"{tmp}/escalated.json", "w") as f:
    json.dump([entry, ["not", "an entry"], "junk"], f)
run(86400 + 200)
assert tuple(entry) in published[0] and ("not", "an entry") not in published[0], published
with open(f"{tmp}/escalated.json", "w") as f:
    f.write("{")
run(86400 + 300)
assert not any(x[0] == entry[0] for x in published[0]), published
os.remove(f"{tmp}/escalated.json")
# Without the watchdog, nothing is watched and nothing escalated.
watch, S.CFG["watch"] = S.CFG["watch"], None
os.remove(f"{tmp}/state/watch.json")
C.NOW = T + 86400 + 400
sys.argv, argv = ["steward"], sys.argv
S.main()
sys.argv = argv
assert not os.path.exists(f"{tmp}/state/watch.json") and waits.escalated() == []
S.CFG["watch"] = watch
print("waits off: ok")

# A Renovate PR settled green with automerge on waits on Renovate's next run
# on the repo to merge it: overdue an hour on, and the watchdog's kick is
# that run. One without automerge waits as long as any.
dep = {"branch": "renovate/foo-1.x", "title": "Update foo to v1.5"}
auto = "### Configuration\n\n📅 **Schedule**: At any time.\n\n🚦 **Automerge**: Enabled.\n"
manual = auto.replace("Enabled.", "Disabled by config. Please merge this manually once you are satisfied.")
fresh_pr_state()
OPEN["sam/r"], OPEN["sam/q"] = [pr("sam/r", 9, body=auto, **dep), pr("sam/r", 10, body=manual, **dep)], []
items = run(450)
nine, ten = items["sam/r#9"], items["sam/r#10"]
assert nine["phase"] == "Dependency update, CI green" and nine["kick"] == "renovate/sam/r", nine
assert nine["since"] == T + 450 and nine["due"] == T + 450 + 60 * MIN, nine
assert ten["phase"] == nine["phase"] and ten["due"] == T + 450 + 120 * MIN and ten["kick"] == "autoland/sam/r", ten
# Without Renovate on this machine there's no run to kick: its pass is.
S.CFG["watch"]["renovateSweep"] = False
assert run(460, "sam/r")["sam/r#9"]["kick"] == "autoland/sam/r"
S.CFG["watch"]["renovateSweep"] = True
print("waits on Renovate: ok")

S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.watch_default, S.branches, S.V.snap_name = kept_fns
OPEN.clear()
fresh_pr_state()
