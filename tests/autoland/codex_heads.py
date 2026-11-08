"""An engine never judges the head it wrote. Whether one did is read from
every commit on the head, through burrow-upkeep's `commits` (the real one,
on GitHub's compare as harness.serve_compare has it): past GitHub's 250 too,
and where GitHub won't list them all, the PR is judged as a Codex one."""

import json

from harness import COMPARE, C, JOBS, S, clean, commit, compare_calls, compare_pages, fresh_pr_state, head, merged, \
    mkpr, pr_state, queued, run, serve_compare, worker
from touches import DEPENDABOT_EMAIL, codex_written, provenance

N = mkpr()["number"]


def asked(f, pr, st):
    """f on the PR, as a pass asks it: acting for its owner."""
    with C.acting_for("sam/r"):
        return f("sam/r", pr, st)


# A Codex-written commit on a clean head gets the ordinary reviews, then a
# Claude judge. Only the whole trailer counts: a quoted Engine line in prose,
# another key or more after it is no mark, and lands as before.
for message, judge in [("change: code\n\nEngine: codex\n", True),
                       ("change: code\n\neNgInE: codex\n", True),
                       ("change: code\n\nOriginal-engine: codex\n", False),
                       ("change: code\n\nEngine: codex, once\n", False),
                       ("change: prose\n\nEngine: codex\n\nThat was a quoted line.\n", False),
                       ("change: code\n", False)]:
    fresh_pr_state()
    # The trailer need not be on the last commit of the PR.
    COMPARE[N] = compare_pages([commit("e" * 40, message), commit(head, "change: later")])
    run(mkpr(), 0)
    run(mkpr(), 600)
    assert queued() == ["codex", "rival"]
    worker()
    status, _ = run(mkpr(), 650)
    assert merged() is not judge, (status, message)
    assert bool(JOBS) is judge, (status, JOBS)
    if judge:
        assert "a Codex job wrote it" in status and "judging" in status, status

# A PR past GitHub's 250-commit list is read whole, a page at a time: a
# Codex trailer on its 251st commit, the head, counts, and one with none
# lands on its reviews.
many = [commit("%040x" % i, "change: earlier") for i in range(250)]
for message, written in [("change: code\n\nEngine: codex\n", True), ("change: code", False)]:
    fresh_pr_state()
    COMPARE[N] = compare_pages(many + [commit(head, message)])
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()
    status, _ = run(clean(), 650)
    assert merged() is not written and bool(JOBS) is written, (status, JOBS)
    assert len(compare_calls()) == 3, compare_calls()

# An unreadable list waits without starting a judge or landing: a failed
# read, a list without the head, a commit with no message, or pages that
# disagree because the base moved between them. Failures aren't kept; a
# later pass reads the commits and lands normally.
moved = compare_pages([commit("%040x" % i, "change: code") for i in range(150)] + [commit(head, "change: code")])
for unreadable in [None, compare_pages([commit("b" * 40, "change: code")]),
                   compare_pages([{"sha": head, "commit": {}}]),
                   [moved[0], {**moved[1], "merge_base_commit": {"sha": "d" * 40}}],
                   [moved[0], {**moved[1], "total_commits": 152}]]:
    fresh_pr_state()
    COMPARE.pop(N, None)
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()
    COMPARE[N] = unreadable
    for at in (650, 710):
        status, _ = run(clean(), at)
        assert "state=pending" in status and "couldn't read its commits; next pass" in status, status
        assert not merged() and not JOBS, (status, JOBS)
        assert "commitsHead" not in pr_state()
    COMPARE.pop(N)
    run(clean(), 720)
    assert merged() and not JOBS

# Where GitHub stops short of its own count for good (the head and merge
# base fixed, the range can't change), no one can say a Codex job didn't
# write it: it's judged, never landed on its reviews alone, and that's kept
# for the head rather than asked every pass.
fresh_pr_state()
COMPARE[N] = compare_pages(many + [commit("%040x" % (250 + i), "change: later") for i in range(49)]
                           + [commit(head, "change: code")], shown=250)
run(mkpr(), 0)
run(mkpr(), 600)
worker()
status, _ = run(clean(), 650)
assert not merged() and JOBS and "250 of its 300 commits" in status, (status, JOBS)
assert len(compare_calls()) == 3
status, _ = run(clean(), 700)
assert compare_calls() == [] and "250 of its 300 commits" in status, (compare_calls(), status)
COMPARE.pop(N)

# Read once per head, base and bots' addresses, across saved-state
# reloads, whatever it said; another head or base reads again.
for written in (False, True):
    st = {}
    for sha, base in [(head, "main"), ("b" * 40, "main"), ("b" * 40, "release")]:
        pr = mkpr(sha=sha, base=base)
        serve_compare(compare_pages([commit("e" * 40, "change: earlier"),
                                     commit(sha, "change: code\n\nEngine: codex\n" if written else "change: code")]))
        assert asked(codex_written, pr, st) is written
        st = json.loads(json.dumps(st))
        assert asked(codex_written, pr, st) is written
        assert len(compare_calls()) == 1, compare_calls()

# What the steward kept of a head's commits before is read again, and goes.
st = {"commitHead": [head, "main"], "commitPages": [[]], "codexHead": [head, "main"], "codexWritten": False,
      "editedHead": [head, "main"], "edited": False}
serve_compare(compare_pages([commit(head, "change: code\n\nEngine: codex\n")]))
assert asked(codex_written, mkpr(), st) is True and set(st) == {"commitsHead", "commits"}, st

# Who made a dependency bot's commits, from the same read: a Claude job's
# co-author line on a Dependabot branch's 251st commit is an edit, and so is
# one that names no address, on a machine with no Renovate address too.
dependabot = mkpr(branch="dependabot/npm/foo-2", title="Bump foo from 1.4 to 1.5")
bot = [commit("%040x" % i, "deps: bump", DEPENDABOT_EMAIL) for i in range(250)]
email = S.CFG["renovateEmail"]
for renovate in (email, None):
    S.CFG["renovateEmail"] = renovate
    for trailer, edited in [("", False), ("\n\nCo-authored-by: Claude <claude@remi>\n", True),
                            ("\n\nCo-authored-by: Claude\n", True)]:
        serve_compare(compare_pages(bot + [commit(head, "deps: bump" + trailer, DEPENDABOT_EMAIL)]))
        assert asked(provenance, dependabot, {}) is edited, (renovate, trailer)
S.CFG["renovateEmail"] = email
# What GitHub won't list whole is unknown.
serve_compare(compare_pages(bot + [commit(head, "deps: bump", DEPENDABOT_EMAIL)], total=300))
assert asked(provenance, dependabot, {}) is None
