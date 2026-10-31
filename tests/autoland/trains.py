"""The merge train (train.py): in a repo with it on, a settled PR goes into
the train instead of being merged, and comes out once it isn't settled any
more; its status says where in the train it is, or why the train holds it
out; the train's own status is CI only as the App's failure; a pass ends
with a step of the train unless it has nothing to do; cn gets the App's
tokens from the steward; and the repo's rules require the train's status."""

import contextlib
import importlib
import io
import json
import os

import train as T
import waits
from harness import (C, CI_GREEN, FAIL, IDLE, JOBS, P, R, S, STEWARD, WROTE, clean, cn, cn_log, cn_runs, fresh_pr_state,
                     gh_stub, head, info, judgment, merges, mkpr, run, said, stand_in, tmp)

stand_in()
other = "c" * 40  # another head
CAND = "ABCDEFGHJKMNPQRSTVWXYZ0123"
SHOW = ["train", "show", "sam/r"]
JOIN = ["train", "join", "sam/r", "5", "--head", head]
LEAVE = ["train", "leave", "sam/r", "5", "--why", "unready"]


def train(prs, state="gating", joined=(5,)):
    """A train with one candidate in flight, of `prs`."""
    return {"joined": [{"pr": n} for n in joined], "in_flight": CAND,
            "candidates": [{"candidate": CAND, "state": {"state": state}, "prs": list(prs)}]}


def mark(state, desc, who=STEWARD):
    """The train's status on the head, as the query gives it."""
    return {"__typename": "StatusContext", "context": "conary/train", "state": state, "description": desc, "creator": who}


def seen(**show):
    """A pass's look at the train: what cn shows, and a log from there on."""
    cn(**show)
    T.begin("sam/r")


# Off, the steward merges a settled PR itself and never asks cn.
fresh_pr_state()
cn()
status, _ = run(clean(), 700)
assert merges() and cn_runs() == [], (WROTE, cn_runs())

# On, it puts it in the train at its head instead, and its status says so:
# the train is what lands it.
S.CFG["repos"]["sam/r"] = {"train": True}
fresh_pr_state()
seen()
waits.begin()
status, _ = run(clean(), 700)
assert not merges(), WROTE
assert cn_runs() == [SHOW, JOIN], cn_runs()
assert "state=success" in status and "In the train, next in line." in status, status
# cn writes the ledger's own log, with the App's tokens from this steward.
env = cn_log()[1]["env"]
assert env["CONARY_LEDGER"] == f"{tmp}/ledger" and env["CONARY_ANCHOR_REPO"] == "sam/homelab", env
assert env["CONARY_TOKEN_COMMAND"].endswith("/steward.py token") and " -E -s " in env["CONARY_TOKEN_COMMAND"], env
# What it waits on is the train's, with the train's deadline.
noted = waits.NOTED["sam/r"]["sam/r#5"]
assert noted["phase"].startswith("In the train, next in line.") and noted["minutes"] == 240, noted

# One touching a one-way path (R2), once its judge said land, joins alone: a
# candidate of its own, which lands only while main is still its base.
fresh_pr_state()
seen()
status, _ = run(clean(files=(".github/CODEOWNERS",), comments=[said(judgment("land", n=5))]), 700)
assert cn_runs() == [SHOW, JOIN + ["--alone"]] and "In the train" in status, (cn_runs(), status)

# Once it's in at this head, it isn't joined again, and its status names the
# candidate in flight: its own (gating, or green and landing), or the one
# ahead of it. Each is a wait of its own.
for show, says in ((train([5, 7]), "In the train, gating in candidate ABCDEFGH with #7."),
                   (train([5], "green"), "In the train, landing in candidate ABCDEFGH."),
                   (train([7], joined=(7, 5)), "In the train, behind candidate ABCDEFGH.")):
    seen(show=show)
    status, _ = run(clean(), 800)
    assert cn_runs() == [SHOW] and not merges(), (cn_runs(), WROTE)
    assert says in status, (says, status)
# GitHub working out again whether it can merge it alone (as it does for
# every PR each time the train lands one) is no reason to leave: the train
# merges it itself. Nor is anything GitHub says of merging it alone.
for state in ("UNKNOWN", "SOMETHING_NEW"):
    fresh_pr_state()
    seen(show=train([5, 7]))
    run(clean(), 800)
    seen(show=train([5, 7]))
    status, _ = run({**clean(), "mergeStateStatus": state}, 850)
    assert cn_runs() == [SHOW] and "In the train, gating" in status, (state, cn_runs(), status)
# The ledger says it isn't in (it left, or landed and came back): joined again.
seen()
run(clean(), 900)
assert cn_runs() == [SHOW, JOIN], cn_runs()
# Settled at another head, it joins at that one.
seen(show=train([5]))
run(clean(sha=other), 900)
assert cn_runs() == [SHOW, ["train", "join", "sam/r", "5", "--head", other]], cn_runs()

# No longer settled while it's in (a new finding, a new head not reviewed
# yet, a draft, a hold), it comes out of the train, after its status says so:
# one the train lands meanwhile has no status the rules take.
said = []
kept_leave = T.leave
T.leave = lambda repo, pr: said.append([x for x in WROTE if "/statuses/" in x]) or kept_leave(repo, pr)
for pr in (clean(threads=[False]), mkpr(sha=other), clean(draft=True), clean(labels=["hold"])):
    fresh_pr_state()
    seen(show=train([5]))
    said.clear()
    status, _ = run(pr, 1000)
    assert "state=pending" in status and cn_runs() == [SHOW, LEAVE], (status, cn_runs())
    assert len(said) == 1 and "state=pending" in " ".join(said[0]), said
    assert T.seat("sam/r", 5) is None
# One that isn't in has nothing to leave.
seen()
run(clean(threads=[False]), 1000)
assert cn_runs() == [SHOW], cn_runs()
# While its candidate is landing a PR, cn keeps it in until a step settles
# that, and it's asked again on a later pass.
fresh_pr_state()
seen(show=train([5], "green"), leave={"left": False, "landing": 5})
status, _ = run(clean(threads=[False]), 1000)
assert "state=pending" in status and cn_runs() == [SHOW, LEAVE] and T.seat("sam/r", 5), (status, cn_runs())
T.leave = kept_leave

# Held out of the train, it waits for what lets it back in, and the status
# says which, with what GitHub said when it refused the merge.
for held, checks, says in (("conflict", CI_GREEN, "it won't merge after the PRs ahead of it, until they've landed or left"),
                           ("red", CI_GREEN, "it was red alone in it, until a new head"),
                           ("refused", CI_GREEN + [mark("ERROR", "GitHub refused its merge: 2 of 3 required checks are expected")],
                            "GitHub refused its merge: 2 of 3 required checks are expected, until the default branch moves")):
    fresh_pr_state()
    seen(join={"joined": False, "held": held})
    waits.begin()
    status, _ = run(clean(checks=checks), 700)
    assert "state=pending" in status and f"held out of the train: {says}" in status and not merges(), (held, status)
    assert waits.NOTED["sam/r"]["sam/r#5"]["minutes"] == 240
    # Asked again each pass: the ledger knows when it's let back in.
    seen()
    status, _ = run(clean(checks=checks), 800)
    assert cn_runs() == [SHOW, JOIN] and "In the train" in status, (cn_runs(), status)

# GitHub not taking the status the rules require: out again, until it does.
fresh_pr_state()
seen()
FAIL.append("/statuses/")
run(clean(), 700)
FAIL.clear()
assert cn_runs() == [SHOW, JOIN, LEAVE], cn_runs()

# A merge queued on it would land it past the train: taken off first, and
# while GitHub won't take it off, it stays out.
fresh_pr_state()
seen()
queued = {**clean(), "autoMergeRequest": {"enabledAt": "x"}}
run(queued, 700)
assert "pr merge 5 -R sam/r --disable-auto" in WROTE and cn_runs() == [SHOW, JOIN], (WROTE, cn_runs())
fresh_pr_state()
seen()
FAIL.append("--disable-auto")
status, _ = run(queued, 700)
FAIL.clear()
assert "didn't take its queued merge off" in status and cn_runs() == [SHOW], (status, cn_runs())

# The train's status is its word on the PR, not a check of it: its success
# (posted just before it lands) and its error count for nothing, and nor
# does anyone else's under its name. Its failure, the App's, is red CI.
red = mark("FAILURE", "red alone in candidate ABCDEFGH: ci")
assert P.ci_state(mkpr(checks=[mark("SUCCESS", "landing with candidate ABCDEFGH")])) == ("none", [], False)
assert P.ci_state(mkpr(checks=[mark("ERROR", "GitHub refused its merge: no")])) == ("none", [], False)
assert P.ci_state(mkpr(checks=[mark("FAILURE", "red", who={"login": "sam", "__typename": "User"})])) == ("none", [], False)
assert P.ci_state(mkpr(checks=CI_GREEN + [red])) == ("red", ["conary/train (red alone in candidate ABCDEFGH: ci)"], False)
# A fix job takes it, told what the train's failure means.
fresh_pr_state()
seen()
run(clean(checks=CI_GREEN + [red]), 700)
JOBS.clear()
status, _ = run(clean(checks=CI_GREEN + [red]), 700 + 30 * 60 + 1)
assert "CI is red" in status and len(JOBS) == 1 and "it merged this head alone onto main" in JOBS[0], (status, JOBS)
assert cn_runs() == [SHOW], cn_runs()
# Plain red CI gets the plain job.
fresh_pr_state()
failing = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
run(clean(checks=failing), 700)
run(clean(checks=failing), 700 + 30 * 60 + 1)
assert len(JOBS) == 1 and "merge train" not in JOBS[0], JOBS

# With no CI on the PR at all, the train's CI is what tests it: it joins. A
# repo without its train judges such a PR first.
fresh_pr_state()
seen()
status, _ = run(clean(checks=()), 700)
assert cn_runs() == [SHOW, JOIN] and "In the train" in status, (cn_runs(), status)
del S.CFG["repos"]["sam/r"]
fresh_pr_state()
seen()
status, _ = run(clean(checks=()), 700)
assert "the repo has no CI" in status and cn_runs() == [], (status, cn_runs())
S.CFG["repos"]["sam/r"] = {"train": True}

# A Renovate minor goes through the train too: Renovate can't merge it past
# the rules that require the train's status.
fresh_pr_state()
seen()
status, _ = run(mkpr(branch="renovate/foo-1.x", title="Update foo from 1.4 to 1.5"), 900)
assert cn_runs() == [SHOW, JOIN] and "In the train" in status, (cn_runs(), status)

# The train lands only onto the default branch: a PR against another one
# merges as it would without it, judged first with no CI. Renovate merges
# nothing in a train repo, so its own there is the steward's to merge. One
# retargeted since it joined comes out first.
for pr, says, merged_here in (
        (clean(base="release"), "state=success", True), (clean(base="release", checks=()), "the repo has no CI", False),
        (mkpr(branch="renovate/foo-1.x", title="Update foo from 1.4 to 1.5", base="release"),
         "state=success", True)):
    fresh_pr_state()
    seen()
    status, _ = run(pr, 900)
    assert cn_runs() == [SHOW] and says in status and "train" not in status, (cn_runs(), status)
    assert bool(merges()) == merged_here, (says, WROTE)
fresh_pr_state()
seen(show=train([5]))
run(clean(base="release"), 900)
assert merges() and cn_runs() == [SHOW, LEAVE] and T.seat("sam/r", 5) is None, (WROTE, cn_runs())
# Retargeted between the steward's look and cn's: held out, and seen to again.
fresh_pr_state()
seen(join={"joined": False, "targets": "release"})
status, _ = run(clean(), 700)
assert "held out of the train: it was just retargeted off the default branch" in status and not merges(), status
print("train PRs: ok")

# A pass ends with a step of the train, after the PRs (so one settled in it
# is in the candidate it builds), on the repo's default branch.
kept_fns = (S.R.discover, S.R.settle_repo, S.open_prs, S.R.publish, S.R.watch_default)
S.R.settle_repo = lambda r, known: False
S.R.discover = lambda: [{"repo": "sam/r", "private": False, "default": "trunk"}]
trunk = {**info, "defaultBranchRef": {"name": "trunk", "target": {"oid": "d" * 40, "statusCheckRollup": {"state": "SUCCESS"}}}}
PRS = {"v": [clean(base="trunk")]}
S.open_prs = lambda owner, name: (trunk, PRS["v"])
S.R.publish = lambda lines: None
S.R.watch_default = lambda r, info, prs, digest: set()
STEP = ["train", "step", "sam/r", "--size", "8"]


def kicked(**do):
    fresh_pr_state()
    cn(**do)
    C.NOW = C.ts("2026-10-24T00:00:00Z") + 700
    C.WAKE.clear()
    S.run_pass(["sam/r"])
    return cn_runs()


assert kicked() == [SHOW, ["train", "join", "sam/r", "5", "--head", head], STEP], cn_runs()
# Its size and the checks its CI waits for are the repo's.
S.CFG["repos"]["sam/r"].update(trainSize=3, trainRequire=["gate", "e2e"])
assert kicked()[-1] == ["train", "step", "sam/r", "--size", "3", "--require", "gate", "--require", "e2e"]
S.CFG["repos"]["sam/r"] = {"train": True}
# Nothing in it, nothing in flight and the last candidate finished: no step.
PRS["v"] = [mkpr(base="trunk")]  # not reviewed yet
assert kicked() == [SHOW], cn_runs()
done = {"joined": [], "in_flight": None, "candidates": [{"candidate": CAND, "state": {"state": "finished"}, "prs": [3]}]}
assert kicked(show=done) == [SHOW], cn_runs()
# A candidate that landed but isn't finished yet, or one in flight, is.
landed = {**done, "candidates": [{**done["candidates"][0], "state": {"state": "landed"}}]}
assert kicked(show=landed) == [SHOW, STEP], cn_runs()
assert kicked(show={**done, "in_flight": CAND}) == [SHOW, STEP], cn_runs()
# GitHub unavailable: another pass in five minutes, and no stall. Any other
# failure is the steward's pass failing on the repo, for the watchdog.
PRS["v"] = [clean(base="trunk")]
kicked(show=landed, rc={"step": 75})
assert C.WAKE["sam/r"] == C.NOW + 5 * 60 and "sam/r" not in waits.FAILED, (C.WAKE, waits.FAILED)
kicked(show=landed, rc={"step": 1})
assert C.WAKE["sam/r"] == C.NOW + 5 * 60 and "sam/r" in waits.FAILED, (C.WAKE, waits.FAILED)
# A PR in it that isn't settled any more, whose status GitHub didn't take,
# and that cn keeps in while its candidate is landing: its old status stands
# for cn, so the train isn't stepped this pass. With the status taken, cn
# reads it and takes the PR out instead of landing it (Conary#55), and it is.
PRS["v"] = [clean(base="trunk", threads=[False])]
KEEP = {"show": train([5], "green"), "leave": {"left": False, "landing": 5}}
FAIL.append("/statuses/")
assert kicked(**KEEP)[-1] == LEAVE and "sam/r" in T.STOPPED, cn_runs()
FAIL.clear()
assert kicked(**KEEP)[-1] == STEP, cn_runs()
# The step says which it took out, as it says which it merged.
out = io.StringIO()
with contextlib.redirect_stdout(out):
    kicked(show=train([5], "green"), step={**IDLE, "landed": [4], "unready": [5]})
assert "sam/r: train: #4 merged; #5 out, no longer settled" in out.getvalue(), out.getvalue()
# Nor is it stepped when a PR in it wasn't seen to at all: it may not be
# settled any more. One that isn't in it stops nothing.
kept_handle = S.step.handle
S.step.handle = lambda *a: (_ for _ in ()).throw(RuntimeError("GitHub said 502"))
assert kicked(show=train([5]))[-1] == SHOW, cn_runs()
assert kicked(show=train([9], joined=(9,)))[-1] == STEP, cn_runs()
S.step.handle = kept_handle
# The train off: none of it.
del S.CFG["repos"]["sam/r"]
assert kicked(show=landed) == [], cn_runs()
S.R.discover, S.R.settle_repo, S.open_prs, S.R.publish, S.R.watch_default = kept_fns
print("train step: ok")

# cn's token command is the steward's `token`, which prints the App's token
# for the repo's owner and nothing else, and exits 75 when GitHub can't be
# asked, as cn does.
out = io.StringIO()
with contextlib.redirect_stdout(out):
    S.token("Sam/r")
assert out.getvalue() == "ghs_app_sam\n", out.getvalue()
kept_token = C.token
for e, code in ((C.Unavailable("HTTP 502"), 75), (RuntimeError("the App isn't installed on sam"), 1)):
    C.token = lambda owner, e=e: (_ for _ in ()).throw(e)
    out = io.StringIO()
    try:
        with contextlib.redirect_stdout(out):
            S.token("sam/r")
        raise AssertionError("printed a token it didn't have")
    except SystemExit as x:
        assert x.code == code and out.getvalue() == "", (x.code, out.getvalue())
C.token = kept_token
# Run by cn as cn runs it (the repo added last), it's the steward's own
# command, taking no lock: here with no App key, so no token and no output.
nokey = f"{tmp}/no-key"
os.makedirs(nokey)
cred, os.environ["CREDENTIALS_DIRECTORY"] = os.environ["CREDENTIALS_DIRECTORY"], nokey
S.CFG["repos"]["sam/r"] = {"train": True}
seen(token=True)
del S.CFG["repos"]["sam/r"]
os.environ["CREDENTIALS_DIRECTORY"] = cred
ran = cn_log()[0]["token"]
assert ran["rc"] == 1 and ran["out"] == "" and "no token for sam/r: couldn't sign as the App" in ran["err"], ran
print("train token: ok")

# The baseline ruleset requires the train's status, the App's alone, in a
# repo with its train on; one already there is brought in line when the
# train goes on or off, and another ruleset only has it pinned to the App.
rc = C.repo_cfg("sam/r")
assert R.required(False) == [{"context": "review-settled", "integration_id": 7}]
assert R.required(True) == [{"context": "review-settled", "integration_id": 7}, {"context": "conary/train", "integration_id": 7}]
made = R.baseline_ruleset({**rc, "train": True})
assert [y for y in made["rules"] if y["type"] == "required_status_checks"][0]["parameters"]["required_status_checks"] == R.required(True)
made = R.baseline_ruleset(rc)
assert [y for y in made["rules"] if y["type"] == "required_status_checks"][0]["parameters"]["required_status_checks"] == R.required(False)
RULES = {}
PUT = []


def rules_gh(*a, **k):
    return RULES.get(a[1]) if a[:1] == ("api",) else None


def rules_write(*a, **k):
    PUT.append((a[3], k.get("input")))
    return {}


OWNER_BYPASS = [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request"}]


def ruleset(name, checks, bypass=()):
    return {"name": name, "target": "branch", "enforcement": "active", "conditions": {}, "bypass_actors": list(bypass),
            "rules": [{"type": "required_status_checks", "parameters": {
                "strict_required_status_checks_policy": False, "required_status_checks": checks}},
                      {"type": "deletion"}, {"type": "non_fast_forward"}, {"type": "pull_request", "parameters": {}}]}


kept_gh, kept_write = C.gh, C.write
C.gh, C.write = rules_gh, rules_write
settled = [{"context": "review-settled", "integration_id": 7}]
for on, checks, want in ((True, settled, R.required(True)),
                         (False, R.required(True), R.required(False)),
                         (True, R.required(True), None), (False, settled, None)):
    RULES["repos/sam/r/rulesets/1"] = ruleset("baseline", checks, () if on else OWNER_BYPASS)
    PUT.clear()
    assert R.keep_rules("sam/r", {**rc, "train": on}, [{"id": 1}])
    if want is None:
        assert PUT == [], (on, checks, PUT)
    else:
        assert len(PUT) == 1 and json.loads(PUT[0][1])["rules"][0]["parameters"]["required_status_checks"] == want, (on, PUT)
# A baseline from before the train (remi's, made while it pushed directly:
# only the first two rules, the owner bypassing always) has no required
# checks to add the train's to: it gets the rules it lacks, and no bypass.
old = ruleset("baseline", [])
old["rules"] = [{"type": "deletion"}, {"type": "non_fast_forward"}]
old["bypass_actors"] = [{**OWNER_BYPASS[0], "bypass_mode": "always"}]
RULES["repos/sam/r/rulesets/1"] = old
PUT.clear()
assert R.keep_rules("sam/r", {**rc, "train": True}, [{"id": 1}])
sent = json.loads(PUT[0][1])
assert [y["type"] for y in sent["rules"]] == ["deletion", "non_fast_forward", "pull_request", "required_status_checks"], sent
assert sent["rules"][3]["parameters"]["required_status_checks"] == R.required(True) and sent["bypass_actors"] == [], sent
# Its bypass comes back with the train off, a directPush repo's rules stay
# as they are, and so does any baseline with rulesets off.
RULES["repos/sam/r/rulesets/1"] = ruleset("baseline", R.required(False))
PUT.clear()
assert R.keep_rules("sam/r", {**rc, "train": False}, [{"id": 1}])
assert json.loads(PUT[0][1])["bypass_actors"] == OWNER_BYPASS, PUT
for changed in ({"directPush": True}, {"ruleset": False}):
    RULES["repos/sam/r/rulesets/1"] = {**old, "rules": [{"type": "deletion"}]}
    PUT.clear()
    assert R.keep_rules("sam/r", {**rc, "train": False, **changed}, [{"id": 1}]) and PUT == [], (changed, PUT)
# Given up with the ledger off as well, when there's no train for cn to run:
# the status's name isn't the ledger's to say.
on_rules, off_rules = R.required(True), R.required(False)
kept_train, C.CFG["train"] = C.CFG["train"], None
importlib.reload(T)
RULES["repos/sam/r/rulesets/1"] = ruleset("baseline", on_rules)
PUT.clear()
assert R.keep_rules("sam/r", {**rc, "train": False}, [{"id": 1}])
assert len(PUT) == 1 and json.loads(PUT[0][1])["rules"][0]["parameters"]["required_status_checks"] == off_rules, PUT
C.CFG["train"] = kept_train
importlib.reload(T)
RULES["repos/sam/r/rulesets/2"] = ruleset("gates", [{"context": "conary/train"}])
PUT.clear()
assert R.keep_rules("sam/r", {**rc, "train": False}, [{"id": 2}])
assert json.loads(PUT[0][1])["rules"][0]["parameters"]["required_status_checks"] == [
    {"context": "conary/train", "integration_id": 7}], PUT

# The repo's settings are due as soon as its train goes on or off, not a
# day later: the baseline requires the train's status only while it's on.
calls = []
C.gh = lambda *a, **k: calls.append(a[:2]) or gh_stub(*a, **k)
S.CFG["repos"]["sam/r"] = {"train": True}
for was, due in ((True, False), (False, True)):
    fresh_pr_state()
    C.save("repo-sam_r.json", {"settings": C.NOW, "app": 7, "train": was, "labels": ["hold", "needs-owner"]})
    calls.clear()
    with C.acting_for("sam"):
        R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
    assert (("api", "repos/sam/r") in calls) == due, (was, calls)

# The branch watch leaves the train's candidates' branches to cn.
BRANCHES = [{"name": "conary/train/" + CAND, "commit": {"sha": "e" * 40}}, {"name": "old", "commit": {"sha": "f" * 40}}]
deleted = []
kept_watch = (R.closed_at, R.delete_branch, C.fresh, C.NOW)
R.closed_at = lambda repo, name, sha: 0
R.delete_branch = lambda repo, name, sha: deleted.append(name)
C.fresh = lambda path, etag=None: ("tag", BRANCHES if "/branches?" in path else [])  # and no PR closed
C.gh = lambda *a, **k: {"ahead": 0}
for on, want in ((True, ["old"]), (False, ["conary/train/" + CAND, "old"])):
    S.CFG["repos"]["sam/r"] = {"train": on}
    fresh_pr_state()
    deleted.clear()
    C.NOW = kept_watch[3]
    R.watch_branches({"repo": "sam/r", "default": "main"}, set(), [])
    C.NOW += R.GRACE  # each goes once it has sat that long
    R.watch_branches({"repo": "sam/r", "default": "main"}, set(), [])
    assert deleted == want, (on, deleted)
del S.CFG["repos"]["sam/r"]
R.closed_at, R.delete_branch, C.fresh, C.NOW = kept_watch
C.gh, C.write = kept_gh, kept_write
fresh_pr_state()
print("train rules: ok")
