"""A late review is asked for as the owner, through the owner's door
(burrow-review-ask), never as the steward's App: Codex's connector, as it
was seen on 2026-11-19 (#1216), reviews a head on an ask only when the ask
is the owner's. Once a head, and the digest only once that ask too went
unanswered; nothing where the repo isn't the door's."""

import os
import shutil
import stat

from harness import (DOOR, OWNER_ASKS, REAL, WROTE, C, S, T, T0, found, fresh_pr_state, head, mkpr, pr_state, run,
                     stand_in, summary, tmp)

stand_in()

# The asks each side made, across passes.
heard = {"app": [], "owner": []}


def step(pr, after):
    status, digest = run(pr, after)
    heard["app"] += [x for x in WROTE if "/comments" in x and "@codex" in x]
    heard["owner"] += OWNER_ASKS
    return status, digest


def connector(pr):
    """The PR as Codex's connector leaves it: a head the owner asked about is
    under review (its summary says so within a minute); an App's ask, as
    anyone else's, it leaves alone."""
    if any(h == pr["headRefOid"] for (_, _, h) in heard["owner"]):
        return {**pr, "comments": {**pr["comments"], "nodes": [*pr["comments"]["nodes"], summary(status="running", at=T0)]}}
    return pr


def fresh():
    fresh_pr_state()
    heard["app"].clear()
    heard["owner"].clear()


# A head with no review QUIET minutes after its push is asked for once, as
# the owner, and the connector takes it: the next pass sees it reviewing.
# The App asks nothing.
fresh()
step(mkpr(), 0)
step(mkpr(), 30 * 60)
assert heard["owner"] == [("sam/r", 5, head)] and heard["app"] == [], heard
status, _ = step(connector(mkpr()), 31 * 60)
assert "Codex is reviewing this commit" in status, status
print("the owner's ask brings the review: ok")

# Once a head: passes after it ask nothing more, however long the review
# takes; the owner hears of it at LOST, saying it was asked and when.
fresh()
step(mkpr(), 0)
step(mkpr(), 30 * 60)
for at in (31, 60, 90):
    _, digest = step(mkpr(), at * 60)
    assert digest == [], digest
assert len(heard["owner"]) == 1 and heard["app"] == [], heard
status, digest = step(mkpr(), 120 * 60)
assert [d[0] for d in digest] == ["Codex hasn't reviewed it"] and "asked as the owner at" in digest[0][2], digest
assert "unanswered" in status and len(heard["owner"]) == 1, (status, heard)
# A new head is asked for again.
other = "b" * 40
step(mkpr(sha=other), 0)
step(mkpr(sha=other), 30 * 60)
assert heard["owner"][-1] == ("sam/r", 5, other), heard

# Asked late (a head first looked at well after its push), the digest waits
# out the ask's own ANSWERED minutes too.
fresh()
early = [{"createdAt": "2026-10-23T22:20:00Z", "conclusion": None, "workflowRun": None}]  # 100 minutes before T0
_, digest = step(mkpr(suites=early), 0)
assert len(heard["owner"]) == 1 and digest == [], (heard, digest)
_, digest = step(mkpr(suites=early), 25 * 60)
assert digest == [] and C.WAKE == {"sam/r": T + 30 * 60 + 1}, (digest, C.WAKE)
_, digest = step(mkpr(suites=early), 30 * 60)
assert [d[0] for d in digest] == ["Codex hasn't reviewed it"], digest

# A repo that isn't the door's is never asked for, by the door or the App:
# the owner hears of it at LOST, saying why it wasn't asked.
fresh()
repos, S.CFG["askAsOwner"]["repos"] = S.CFG["askAsOwner"]["repos"], []
step(mkpr(), 0)
step(mkpr(), 30 * 60)
status, digest = step(mkpr(), 120 * 60)
assert heard == {"app": [], "owner": []}, heard
assert "not asked as the owner: this repo isn't one the owner's door asks in" in digest[0][2], digest
# No door at all: the same.
door, S.CFG["askAsOwner"] = S.CFG["askAsOwner"], None
step(mkpr(), 30 * 60)
assert heard == {"app": [], "owner": []}, heard
S.CFG["askAsOwner"] = door
S.CFG["askAsOwner"]["repos"] = repos

# What the door's own checks decline (a hold, a draft, the day's cap) is no
# failure: no strike, and the owner hears why at LOST. A door that failed
# is a strike, as GitHub's refusals are.
fresh()
DOOR.update(outcome="declined", why="it's labelled hold")
step(mkpr(), 0)
step(mkpr(), 30 * 60)
assert "ask" not in pr_state().get("strikes", {}), pr_state()
_, digest = step(mkpr(), 120 * 60)
assert "not asked as the owner: it's labelled hold" in digest[0][2], digest
fresh()
DOOR.update(outcome="failed", why="GitHub refused the comment (HTTP 403)")
step(mkpr(), 0)
step(mkpr(), 30 * 60)
assert pr_state()["strikes"]["ask"] == 1, pr_state()
DOOR.clear()
step(mkpr(), 31 * 60)
assert "ask" not in pr_state().get("strikes", {}) and len(heard["owner"]) == 1, (pr_state(), heard)

# Findings answered without a push: once the settle delay is up, asked for
# once, as the owner, for what Codex last said; a look that doesn't come
# within ANSWERED minutes goes to the owner. A new word from Codex is
# another thing to ask about.
fresh()
answered = mkpr(reviews=[found()], comments=[summary(findings=True)], threads=[True])
step(answered, 700)
assert heard["owner"] == [], heard
step(answered, 900)
step(answered, 960)
assert heard["owner"] == [("sam/r", 5, head)] and heard["app"] == [], heard
_, digest = step(answered, 900 + 29 * 60)
assert digest == [], digest
status, digest = step(answered, 900 + 30 * 60)
assert [d[0] for d in digest] == ["Codex hasn't looked again"] and "asked as the owner" in digest[0][2], digest
again = mkpr(reviews=[found(), found(at="2026-10-24T00:50:00Z")],
             comments=[summary(findings=True, at="2026-10-24T00:50:00Z")], threads=[True, True])
step(again, 3600)
assert len(heard["owner"]) == 2, heard
print("once a head, the digest after: ok")

# The door runs as a unit of the machine user's own, waited for, with only
# the PR it's for: what it prints is what came of it.
bin_dir = f"{tmp}/owner-bin"
os.makedirs(bin_dir, exist_ok=True)
with open(f"{bin_dir}/systemd-run", "w") as f:
    f.write(f"""#!{shutil.which("bash")}
printf '%s\\n' "$@" > {tmp}/systemd-run-args
echo '{{"outcome": "posted", "at": 7}}'
""")
os.chmod(f"{bin_dir}/systemd-run", stat.S_IRWXU)
path = os.environ["PATH"]
os.environ["PATH"] = f"{bin_dir}:{path}"
assert REAL["ask_as_owner"]("sam/r", 5, head) == {"outcome": "posted", "at": 7}
args = open(f"{tmp}/systemd-run-args").read().split("\n")[:-1]
assert args[:5] == ["--user", "--wait", "--pipe", "--quiet", "--collect"] and args[5].startswith("--unit=autoland-ask-"), args
assert args[6:] == ["--", f"{tmp}/review-ask", "sam/r", "5", head], args
with open(f"{bin_dir}/systemd-run", "w") as f:
    f.write(f"#!{shutil.which('bash')}\necho 'Failed to connect to bus' >&2\nexit 1\n")
out = REAL["ask_as_owner"]("sam/r", 5, head)
assert out["outcome"] == "failed" and "Failed to connect to bus" in out["why"], out
os.environ["PATH"] = path
print("the owner's door: ok")
