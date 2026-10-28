"""The reviewers' answers through handle(): one with findings beside a clean
one, failed and lost ones, one out of service, and the sweep of what no open
PR needs any more."""

import fcntl
import json
import os

from harness import (C, DDIR, POSTED, QDIR, S, STUB, T0, V, W, agent, agent_runs, clean, fresh_pr_state, head, info,
                     merged, mkpr, pr_state, queued, run, rv_review, stand_in, tmp, worker)

stand_in()

# Codex clean but Rival has findings: nothing merges until they're settled.
cx = rv_review("codex", 0)
ds2 = rv_review("rival", 2)
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
assert queued() == ["rival"]
worker(rival={"result": "review", "answer": {"findings": [{"path": "a.py"}, {"path": "a.py"}]}})
status, _ = run(mkpr(reviews=[cx]), 650)
assert POSTED == [("rival", head, 2)] and not merged(), (POSTED, status)
status, _ = run(mkpr(reviews=[ds2, cx], threads=[False, False]), 700)
assert "Round 1: 2 open review threads" in status and not merged()

# Its findings answered and resolved without a new commit: it looks again.
status, _ = run(mkpr(reviews=[ds2, cx], threads=[True, True]), 3600)
assert queued() == ["rival"] and "Findings answered" in status
status, _ = run(mkpr(reviews=[ds2, cx], threads=[True, True]), 3700)
assert queued() == ["rival"]
worker()
# Its new review, later than the ask, is what counts.
status, _ = run(mkpr(reviews=[ds2, cx], threads=[True, True]), 4300)
assert "state=success" in status and merged(), status

# Its second look failing three times: the owner hears, though a review
# with findings is on record.
fresh_pr_state()
answered = mkpr(reviews=[ds2, cx], threads=[True, True])
t = 5000
run(answered, t)
for i in range(3):
    assert queued() == ["rival"], i
    worker(rival={"result": "error", "error": "rival exited 1"})
    t += 10
    run(answered, t)  # its failure comes back
    t += 300
    if i < 2:
        run(answered, t)  # and five minutes on it's asked again
status, digest = run(answered, 6000)
assert any(x[0] == "Rival couldn't review it" for x in digest), digest
assert "Findings answered; Rival failed 3 times" in status and queued() == [], status

# An answer that's no review is a failed one, never a clean one.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
worker(rival={"result": "review", "answer": {"summary": "couldn't finish"}})
status, _ = run(mkpr(reviews=[cx]), 610)
assert POSTED == [] and not merged() and pr_state()["rv"]["rival"]["fails"] == 1, status
assert queued() == [] and os.listdir(DDIR) == []
run(mkpr(reviews=[cx]), 910)
assert queued() == ["rival"]
# One GitHub won't take stays where it is and is posted by the next pass.
worker()
STUB["post"] = RuntimeError("GitHub didn't take the review")
status, _ = run(mkpr(reviews=[cx]), 920)
assert len(os.listdir(DDIR)) == 1 and pr_state()["rv"]["rival"]["fails"] == 2 and not merged(), status
STUB["post"] = None
status, _ = run(mkpr(reviews=[cx]), 930)
assert POSTED == [("rival", head, 0)] and os.listdir(DDIR) == [] and merged(), status
# Posted, its earlier failures are behind it: none left to report.
assert "fails" not in pr_state()["rv"]["rival"], pr_state()["rv"]
# Turned down three times, it's dropped and the owner hears, not posted forever.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
worker()
STUB["post"] = RuntimeError("GitHub didn't take the review")
for i in range(3):
    status, digest = run(mkpr(reviews=[cx]), 610 + i)
STUB["post"] = None
assert os.listdir(DDIR) == [] and queued() == [] and pr_state()["rv"]["rival"]["fails"] == 3, status
assert any(x[0] == "Rival couldn't review it" for x in digest) and not merged(), digest
# GitHub not answering (its rate limit, a 5xx) is no fault of the review or
# the reviewer: however many passes it lasts, the answer's kept, no try is
# spent and the owner isn't told the reviewer failed.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
worker()
STUB["post"] = C.Unavailable("gh api -H Accept: API rate limit exceeded for user ID 1")
for i in range(4):
    status, digest = run(mkpr(reviews=[cx]), 610 + i)
    assert len(os.listdir(DDIR)) == 1 and "fails" not in pr_state()["rv"]["rival"], (i, pr_state()["rv"])
    assert f"Posting Rival's review of {head[:7]}" in status and not merged(), status
    assert not any(x[0] == "Rival couldn't review it" for x in digest), digest
STUB["post"] = None
status, _ = run(mkpr(reviews=[cx]), 620)
assert POSTED == [("rival", head, 0)] and os.listdir(DDIR) == [] and merged(), status
# A snapshot that can't be taken is a failed try too.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
STUB["queue"] = RuntimeError("the head moved on")
status, _ = run(mkpr(reviews=[cx]), 600)
assert queued() == [] and pr_state()["rv"]["rival"]["fails"] == 1 and "Waiting for Rival" in status, status
STUB["queue"] = None

# A queued review that leaves the queue with nothing back is lost: seen so on
# two passes, it counts as a failure and is asked again.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
os.remove(f"{QDIR}/{os.listdir(QDIR)[0]}")
run(mkpr(reviews=[cx]), 610)
assert "fails" not in pr_state()["rv"]["rival"] and queued() == []
status, _ = run(mkpr(reviews=[cx]), 620)
assert pr_state()["rv"]["rival"]["fails"] == 1 and queued() == [], status
status, _ = run(mkpr(reviews=[cx]), 920)
assert pr_state()["rv"]["rival"]["fails"] == 1 and queued() == ["rival"], status
# Still queued, however long it takes: no failure, nothing asked twice.
status, _ = run(mkpr(reviews=[cx]), 920 + 2 * 86400)
assert "Rival is reviewing this commit" in status and queued() == ["rival"], status
assert pr_state()["rv"]["rival"]["fails"] == 1

# Rival down: each failure comes back to a pass, and it's queued again five
# minutes after it failed, however many passes come before that (a burst of
# events would spend all three tries in a minute, before a hiccup passes);
# after three it's the pipeline's to look into, and an hour on it's tried
# again.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
for i in range(3):
    worker(rival={"result": "error", "error": "down"})
    status, digest = run(mkpr(reviews=[cx]), 610 + 310 * i)
    if i < 2:
        assert queued() == [] and C.WAKE["sam/r"] == C.ts(T0) + 610 + 310 * i + 300 + 1, (i, C.WAKE)
        for t in (620, 900, 910):
            run(mkpr(reviews=[cx]), 310 * i + t)
            assert queued() == (["rival"] if t == 910 else []), (i, t)
assert any(x[0] == "Rival couldn't review it" for x in digest) and "Waiting for Rival" in status
assert queued() == []
run(mkpr(reviews=[cx]), 1600)
assert queued() == []
run(mkpr(reviews=[cx]), 5000)
assert queued() == ["rival"]

# Out of service (Codex's plan spent): recorded for every PR, which settle on
# the other reviewer meanwhile, and asked again once it's back.
fresh_pr_state()
run(mkpr(), 0)
run(mkpr(), 600)
worker(codex={"result": "out", "why": "its ChatGPT plan's usage is spent", "until": C.ts(T0) + 4200})
status, _ = run(mkpr(), 650)
assert "Rival found nothing" in status and "Codex and" not in status and merged(), status
assert json.load(open(f"{tmp}/state/{V.OUT}"))["codex"]["until"] == C.ts(T0) + 4200
run(mkpr(number=6), 700)
run(mkpr(number=6), 1400)
assert queued() == ["rival"]
run(mkpr(number=6), 4300)
assert queued() == ["codex", "rival"]
# The owner hears one is out, once however many PRs find it so.
fresh_pr_state()
C.save(V.OUT, {"codex": {"why": "it isn't logged in", "until": C.ts(T0) + 86400}})
digest = []
for num in (5, 6):
    S.handle("sam/r", mkpr(number=num), info, digest)
assert [x for x in digest if x[0] == "A reviewer is out of service"] == [
    ("A reviewer is out of service", "Codex", "it isn't logged in")], digest
# Every reviewer out: the owner hears, and nothing is queued or landed.
fresh_pr_state()
C.save(V.OUT, {r: {"why": "spent", "until": C.ts(T0) + 86400} for r in V.NAMES})
status, digest = run(clean(), 600)
assert "No reviewer can review it now (Codex: spent; Rival: spent)" in status, status
assert [x[0] for x in digest] == ["No reviewer can review it"] and queued() == [] and not merged(), digest

# A repo set to Codex alone lands on Codex's word, and a review queued
# before it was taken off Rival isn't sent.
fresh_pr_state()
run(mkpr(reviews=[cx]), 0)
run(mkpr(reviews=[cx]), 600)
name, = os.listdir(QDIR)
S.CFG["repos"]["sam/r"] = {"rival": False}
agent()
W.review(name)
back = json.load(open(f"{DDIR}/{name}"))
assert back["result"] == "error" and "isn't one Rival reviews" in back["error"] and agent_runs() == [], back
status, _ = run(mkpr(reviews=[cx]), 700)
assert "state=success" in status and "Codex found nothing" in status and merged()
del S.CFG["repos"]["sam/r"]

# The sweep: snapshots, queued reviews and answers of heads no open PR has
# any more go, for the repos the pass read (a dash in a name included).
fresh_pr_state()
gone, live_snap = V.snap_name("pr-sam_r-5.json", "d" * 40, "main"), V.snap_name("pr-sam_r-5.json", head, "main")
other_repo, dashed = V.snap_name("pr-sam_q-1.json", "e" * 40, "main"), V.snap_name("pr-sam_lo-k-3.json", "f" * 40, "main")
# Retargeted: the same head on another base is another snapshot.
retargeted = V.snap_name("pr-sam_r-5.json", head, "release/1.x")
assert retargeted != live_snap
for s in (gone, live_snap, other_repo, dashed, retargeted):
    os.makedirs(f"{tmp}/state/{V.SNAP}/{s}")
    C.save(f"{V.SNAP}/{s}.json", {})
    C.save(f"{V.QUEUE}/rival--{s}.json", {})
    C.save(f"{V.DONE}/codex--{s}.json", {})
# PR 5 is still open: an earlier head's look that's under way (its queue
# entry locked, as the reviewers' service holds it) keeps its snapshot, and
# what came back from its earlier heads stays for its next pass to post.
running = V.snap_name("pr-sam_r-5.json", "c" * 40, "main")
os.makedirs(f"{tmp}/state/{V.SNAP}/{running}")
C.save(f"{V.SNAP}/{running}.json", {})
C.save(f"{V.QUEUE}/codex--{running}.json", {})
held = open(f"{tmp}/state/{V.QUEUE}/codex--{running}.json")
fcntl.flock(held, fcntl.LOCK_SH)
V.sweep({"sam_r", "sam_lo-k"}, {live_snap})
held.close()
left = sorted(x for d in (V.SNAP, V.QUEUE, V.DONE) for x in os.listdir(f"{tmp}/state/{d}"))
assert left == sorted([live_snap, live_snap + ".json", other_repo, other_repo + ".json",
                       f"rival--{live_snap}.json", f"rival--{other_repo}.json",
                       f"codex--{live_snap}.json", f"codex--{other_repo}.json",
                       running, running + ".json", f"codex--{running}.json",
                       f"codex--{gone}.json", f"codex--{retargeted}.json"]), left
# Once it's done, the next sweep takes its snapshot.
V.remove(f"{tmp}/state/{V.QUEUE}/codex--{running}.json")
V.sweep({"sam_r", "sam_lo-k"}, {live_snap})
assert not os.path.exists(f"{tmp}/state/{V.SNAP}/{running}") and os.path.exists(f"{tmp}/state/{V.DONE}/codex--{gone}.json")
# And a PR that's gone takes everything with it.
V.sweep({"sam_r", "sam_lo-k"}, set())
left = sorted(x for d in (V.SNAP, V.QUEUE, V.DONE) for x in os.listdir(f"{tmp}/state/{d}"))
assert left == sorted([other_repo, other_repo + ".json", f"rival--{other_repo}.json", f"codex--{other_repo}.json"]), left

print("answers: ok")
