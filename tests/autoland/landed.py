"""A PR whose merge would change nothing on its base is closed as landed,
never merged: its head merged into the base's tip has the tip's own tree,
because another PR made the same change (two fixes for a red main, say).
onto reads that from the steward's mirror, fetched from the repo's real
remote here; handle() closes such a PR with a comment, and leaves a
dependency bot's to its bot, never clearing it to merge. A fix for red main
lands only on the tip the pass saw red."""

from harness import ALREADY, REAL, TIP, WROTE, C, S, clean, cn, cn_runs, fresh_pr_state, info, merged, mkpr, pr_state, run, sh, \
    stand_in, tmp, upkeep_config

import train as T

stand_in()

# A repo whose PRs' heads are real commits on its remote, as GitHub has them.
origin, work = f"{tmp}/landed-origin", f"{tmp}/landed-work"
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
remote = f"{origin}/sam/r.git"
sh("git", "init", "-q", "--bare", remote)
sh("git", "init", "-q", "-b", "main", work)


def put(path, text):
    with open(f"{work}/{path}", "w") as f:
        f.write(text)


def commit(msg, **files):
    for path, text in files.items():
        put(path, text)
    sh(*GIT, "add", "-A", cwd=work)
    sh(*GIT, "commit", "-qm", msg, cwd=work)
    return sh("git", "rev-parse", "HEAD", cwd=work).strip()


def pull(n, rev):
    sh("git", "push", "-q", "-f", remote, f"{rev}:refs/pull/{n}/head", cwd=work)


red = commit("red", **{"agents.nix": "packages.burrow\n", "other.txt": "1\n"})
sh("git", "push", "-q", remote, "main", cwd=work)
# Two fixes for the red, each from it: the same change, on two branches.
sh("git", "checkout", "-q", "-b", "fix-a", cwd=work)
fix_a = commit("fix a", **{"agents.nix": "packages.burrow-agent\n"})
sh("git", "checkout", "-q", "-b", "fix-b", red, cwd=work)
fix_b = commit("fix b", **{"agents.nix": "packages.burrow-agent\n"})
# A third change, and one that changes the same line another way.
sh("git", "checkout", "-q", "-b", "more", red, cwd=work)
more = commit("more", **{"other.txt": "2\n"})
sh("git", "checkout", "-q", "-b", "clash", red, cwd=work)
clash = commit("clash", **{"agents.nix": "packages.burrow-something\n"})
for n, rev in ((1, fix_a), (2, fix_b), (3, more), (4, clash)):
    pull(n, rev)
# The steward's mirror fetches from it (burrow-upkeep's `fetch`).
kept_upkeep, C.CFG["upkeep"] = C.CFG["upkeep"], upkeep_config(f"{tmp}/upkeep-landed.json", git=origin)


def onto(repo, pr):
    with C.acting_for(repo):
        return REAL["onto"](repo, pr)


def pr(n, rev):
    return {"number": n, "headRefOid": rev, "baseRefName": "main"}


# While main is red, every fix adds something to it.
for n, rev in ((1, fix_a), (2, fix_b), (3, more), (4, clash)):
    assert onto("sam/r", pr(n, rev)) == (red, False), n
# The first fix lands (a squash of it): the second one now adds nothing on
# main's new tip. The others still change something, the one that clashes
# with it included (its merge conflicts).
sh("git", "checkout", "-q", "main", cwd=work)
landed = commit("fix a, squashed", **{"agents.nix": "packages.burrow-agent\n"})
sh("git", "push", "-q", remote, "main", cwd=work)
assert onto("sam/r", pr(2, fix_b)) == (landed, True)
assert onto("sam/r", pr(3, more)) == (landed, False)
assert onto("sam/r", pr(4, clash)) == (landed, False)
# Its merge is read onto main as it is: one that adds nothing there adds
# nothing after main moves on with something else too.
later = commit("later", **{"third.txt": "x\n"})
sh("git", "push", "-q", remote, "main", cwd=work)
assert onto("sam/r", pr(2, fix_b)) == (later, True)
# A head already in main (pushed there as it is) adds nothing too.
pull(5, landed)
assert onto("sam/r", pr(5, landed)) == (later, True)
# A PR that moved past the head the pass read changes something: GitHub
# refuses that head's merge anyway.
pull(2, more)
assert onto("sam/r", pr(2, fix_b)) == (later, False)
pull(2, fix_b)
# git that can't fetch it raises: the steward can't tell, so nothing lands.
C.CFG["upkeep"] = upkeep_config(f"{tmp}/upkeep-landed.json", git=f"{tmp}/no-such-origin")
try:
    onto("sam/r", pr(2, fix_b))
    raise AssertionError("a fetch that fails said something")
except RuntimeError as e:
    assert "git -C" in str(e), e
C.CFG["upkeep"] = kept_upkeep
print("onto: ok")

# handle(): a settled PR whose merge would add nothing is closed, saying
# so, and never merged; its status says why.
fresh_pr_state()
ALREADY[5] = later
status, _ = run(clean(), 900)
assert not merged(), WROTE
assert "state=pending" in status and f"Already landed: main at {later[:7]} already has everything it changes; closing it" in status, status
closes = [x for x in WROTE if x.startswith("pr close 5 -R sam/r --comment ")]
assert len(closes) == 1 and f"Closed without merging: main at {later[:7]} already has everything it changes" in closes[0], WROTE
assert not pr_state().get("strikes"), pr_state()
# Behind a base whose rules want it up to date, it isn't brought up to date
# first: the merge of main into it would change nothing either.
fresh_pr_state()
status, _ = run({**clean(), "mergeStateStatus": "BEHIND"}, 900)
assert not any("/update-branch" in x for x in WROTE) and any(x.startswith("pr close 5") for x in WROTE), WROTE
# In a repo with its train on, it doesn't join the train either.
S.CFG["repos"]["sam/r"] = {"train": True}
fresh_pr_state()
cn()
T.begin("sam/r")
run(clean(), 900)
assert not any(a[:2] == ["train", "join"] for a in cn_runs()) and any(x.startswith("pr close 5") for x in WROTE), (cn_runs(), WROTE)
del S.CFG["repos"]["sam/r"]
cn()
# A dependency bot's is left to its bot, which closes its own: closed under
# it, it's an update the bot ignores from then on. That's so for one the
# steward would merge (Dependabot's) and one it would only clear for
# Renovate to merge: it isn't cleared.
for bump in ({"branch": "dependabot/npm/foo-1", "title": "Bump foo from 1.4 to 1.5", "number": 13},
             {"branch": "renovate/foo-1.x", "title": "deps: update foo to v1.5", "number": 14}):
    fresh_pr_state()
    ALREADY[bump["number"]] = later
    status, _ = run(mkpr(**bump), 900)
    assert not merged() and not any(x.startswith("pr close") for x in WROTE), (bump, WROTE)
    assert "state=pending" in status and "Already landed" in status and "its bot closes it" in status, (bump, status)
    # Nor is one whose merge git can't work out.
    ALREADY[bump["number"]] = RuntimeError("git fetch: no network")
    status, _ = run(mkpr(**bump), 1000)
    assert not merged() and "state=pending" in status and "can't be worked out" in status, (bump, status)
ALREADY.clear()
# One that adds something is merged as before, or cleared for Renovate.
fresh_pr_state()
run(clean(), 900)
assert merged() and not any(x.startswith("pr close") for x in WROTE), WROTE
fresh_pr_state()
status, _ = run(mkpr(branch="renovate/foo-1.x", title="deps: update foo to v1.5", number=14), 900)
assert "state=success" in status and "Dependency update, CI green" in status, status
print("landed: ok")

# A fix for red main lands only on the tip the pass saw red: one that moved
# on since (a merge earlier in this very pass, or anyone's push) has no word
# from CI yet, so it waits for it, by the steward or its train.
red_info = {**info, "defaultBranchRef": {"name": "main", "target": {"oid": "f" * 40, "statusCheckRollup": {"state": "FAILURE"}}}}
kept_info = dict(info)
info.update(red_info)
fix = {"branch": "autoland/red-cccccccccccc"}
fresh_pr_state()
TIP["v"] = "e" * 40
status, _ = run(clean(**fix), 900)
assert not merged() and "state=pending" in status and "lands only while main is red" in status, (status, WROTE)
# Any other PR merges onto the moved tip as before: only a red fix waits.
fresh_pr_state()
run(clean(), 900)
assert merged(), WROTE
# On the tip it saw red, the fix lands.
TIP["v"] = "f" * 40
fresh_pr_state()
run(clean(**fix), 900)
assert merged(), WROTE
info.clear()
info.update(kept_info)
print("red fixes on the tip seen red: ok")
