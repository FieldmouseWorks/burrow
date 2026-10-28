"""What the steward waits on, for the stall watchdog (waits.py): every open
PR's wait as its status says it, since when and by when, saved each pass;
which PRs aren't watched; a kicked pass, a failing repo or PR, and what the
watchdog escalated going on the stuck issue."""

import json
import os
import sys

from harness import KICKS, WROTE, T, C, S, fresh_pr_state, info, mkpr, stand_in, tmp

import waits

stand_in()
MIN = 60
published = []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects)
S.R.settle_repo = lambda r, known: False
S.R.publish = published.append
S.R.unhook_gone = lambda repos: None
S.R.hook_projects = lambda repos: []
REPOS = ["sam/r", "sam/q"]
S.R.discover = lambda: [{"repo": r} for r in REPOS]
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
                            "due": T + 1200 + 120 * MIN, "kick": "autoland/sam/r"}, items
assert items["sam/r#5"]["since"] == T + 1100, items
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

S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects = kept_fns
OPEN.clear()
fresh_pr_state()
