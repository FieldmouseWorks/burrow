"""The docs lane: a head burrow's shared CI took in its docs lane for the PR's
diff as it stands (every path it changes since the commit it leaves its base
at is the repo's documentation, nothing its flake builds or tests reads) gets
none of the repo's proofs, and lands on its reviewers and CI alone, saying
so. Only GitHub Actions' passed `docs only into BASE from FORK` check run
counts, for the PR's base and the commit it leaves that base at now, and
only on a PR that changes nothing under .github/, renames out of it
included, with every file it changes read: there the workflow that judged it
is the base branch's. A run that judged another diff (another base: the PR
retargeted since, an old run run again, another PR's on the same head; or a
docs lane from another fork, the base having taken in some of the PR's
commits since) isn't the PR's CI: none of its check runs counts, and until
its workflow has run through for the PR's base, CI is pending. Anything short
of the docs lane runs the proofs as before."""

import pathlib

from harness import FORK, FORKS, JOBS, PR_DIFF, C, S, clean, fresh_pr_state, merged, run

S.CFG["repos"]["sam/r"] = {"proofJobs": {"agents": "Run `nix build .#checks.agents` and post the agents status."}}
PROVING = "a Claude job is running agents"
F, G = FORK["v"], "e" * 40  # where the PR leaves main, and where it once did


def check(name, conclusion="SUCCESS", status="COMPLETED", app="github-actions", suite="s1", workflow="flake",
          done="COMPLETED"):
    return {"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
            "checkSuite": {"id": suite, "status": done, "app": {"slug": app},
                           "workflowRun": {"file": {"path": f".github/workflows/{workflow}.yml"},
                                           "workflow": {"id": workflow, "name": workflow}}}}


def docs(base="main", fork=F, suite="s1", **kw):
    """One run of the shared CI that took the docs lane against base, from fork."""
    return [check(f"check / lane into {base}", suite=suite), check("check / check", "SKIPPED", suite=suite),
            check(f"check / docs only into {base} from {fork}", suite=suite, **kw)]


def whole(base="main", suite="s1", conclusion="SUCCESS", done="COMPLETED"):
    """One that took the whole gate. The docs lane's job, skipped, keeps its
    name unfilled."""
    return [check(f"check / lane into {base}", suite=suite, done=done),
            check("check / check", conclusion, suite=suite, done=done),
            check("check / docs only into ${{ github.event.pull_request.base.ref }} from "
                  "${{ needs.lane.outputs.fork }}", "SKIPPED", suite=suite, done=done)]


def at(pr, t=700):
    """One pass on a fresh PR: its status, and whether it merged."""
    fresh_pr_state()
    status, _ = run(pr, t)
    return status, merged()


# Documentation only, as CI found against the PR's base and fork: no proof,
# and it lands saying why.
status, landed = at(clean(checks=docs(), files=("README.md", "docs/guide.md")))
assert landed and JOBS == [] and "CI is green (docs only)" in status, (status, JOBS)
# Still running: it waits for CI like any check, and no proof starts meanwhile.
status, landed = at(clean(checks=docs(status="IN_PROGRESS", conclusion=None), files=("README.md",)))
assert not landed and JOBS == [] and "waiting for CI" in status, (status, JOBS)
# Red: a fix job has it, as with any red check.
status, landed = at(clean(checks=docs(conclusion="FAILURE"), files=("README.md",)))
assert not landed and "CI is red" in status and PROVING not in status, status

# Anything short of it: the proofs run as before, and it doesn't land yet.
for why, pr in [
    ("the whole gate ran", clean(checks=whole(), files=("README.md",))),
    ("another App's check run", clean(checks=docs(app="some-app"), files=("README.md",))),
    ("a name that only starts the same", clean(checks=docs("main, honest"), files=("README.md",))),
    ("the PR changes its workflows", clean(checks=docs(), files=("README.md", ".github/workflows/flake.yml"))),
    ("the PR changes something else under .github/", clean(checks=docs(), files=(".github/CODEOWNERS",))),
]:
    status, landed = at(pr)
    assert not landed and PROVING in status and len(JOBS) == 1, (why, status, JOBS)
# Files it couldn't all be shown.
pr = clean(checks=docs(), files=("README.md",))
pr["changedFiles"] = 101
status, landed = at(pr)
assert not landed and PROVING in status, status
# A rename out of .github/ counts where it started.
pr = clean(checks=docs(), files=("docs/template.md",))
pr["files"]["nodes"][0]["changeType"] = "RENAMED"
PR_DIFF["v"] = ("diff --git a/.github/pull_request_template.md b/docs/template.md\nsimilarity index 100%\n"
                "rename from .github/pull_request_template.md\nrename to docs/template.md\n")
status, landed = at(pr)
assert not landed and PROVING in status, status
# One whose diff couldn't be read isn't taken on trust either.
PR_DIFF["v"] = ""
status, landed = at(pr)
assert not landed and PROVING in status, status

# A docs lane from another fork: main has taken in some of the PR's commits
# since (one that changed code, which the PR's head undoes, say), so what the
# PR changes now isn't what that run judged. Nothing has run for it since:
# CI is pending, and no proof starts. One whose fork GitHub won't say is the same.
WAIT = "waiting for CI: flake for main"
for fork, why in ((G, "the base took in some of its commits"), (None, "GitHub didn't say")):
    FORK["v"] = fork
    status, landed = at(clean(checks=docs(fork=F), files=("README.md",)))
    assert not landed and JOBS == [] and WAIT in status, (why, status, JOBS)
FORK["v"] = F
# Asked when the PR's turn comes, of the base branch by its name: a PR the
# pass read before another one landed sees that merge. A lands a merge
# commit main takes in, which moves where B (read with A) leaves main, so B's
# docs lane no longer counts; whatever B's snapshot says of the base's tip
# (its baseRefOid) has no say. The same on a branch other than the default
# one, and a merge into another branch moves nothing of B's.
S.CFG["repos"]["sam/r"] = {}
for b_base, a_base in (("main", "main"), ("next", "next"), ("main", "other")):
    a = clean(number=6, checks=whole(a_base), files=("src/a.rs",), base=a_base)
    b = clean(number=7, checks=docs(b_base), files=("README.md",), base=b_base)
    b["baseRefOid"] = "b" * 40
    fresh_pr_state()
    run(a, 700)
    assert merged(), a_base
    FORK[a_base] = G  # what GitHub's compare answers for that branch once it has A's commits
    status, _ = run(b, 700)
    if a_base == b_base:
        assert not merged() and JOBS == [] and f"waiting for CI: flake for {b_base}" in status, (b_base, status)
    else:
        assert merged() and "CI is green (docs only)" in status, (b_base, a_base, status)
    del FORK[a_base]
S.CFG["repos"]["sam/r"] = {"proofJobs": {"agents": "Run `nix build .#checks.agents` and post the agents status."}}
# One decision, one read of where the PR leaves its base: what the step
# waives and what it lands saying are the same answer's, whatever GitHub says
# if asked again (the base taking in some of the PR's commits the moment
# after, say).
for answers, lands in (([F, G, G], True), ([G, F, F], False)):
    FORKS[:] = answers
    status, landed = at(clean(checks=docs(), files=("README.md",)))
    assert landed == lands and JOBS == [] and FORKS == answers[1:], (answers, status, FORKS)
    assert "CI is green (docs only)" in status if lands else "waiting for CI: flake for main" in status, status
FORKS.clear()
# A base branch's name reaches GitHub as git has it, whatever it holds: a
# `#`, a `%`, a slash, a Unicode space. Each is its own branch: one named
# like another's escape (release%2Fnext, release/next's) answers for where
# the PR leaves it, never for where it would leave the other.
for base in ("release/next", "release#1", "50%", "release\u00a0next"):
    status, landed = at(clean(checks=docs(base), files=("README.md",), base=base))
    assert landed and JOBS == [] and "CI is green (docs only)" in status, (base, status, JOBS)
FORK["release%2Fnext"] = G
status, landed = at(clean(checks=docs("release%2Fnext"), files=("README.md",), base="release%2Fnext"))
assert not landed and JOBS == [] and "waiting for CI: flake for release%2Fnext" in status, (status, JOBS)
del FORK["release%2Fnext"]
# So every compare's path is C.compare's: no module writes one itself.
by_hand = [f"{p.name}:{n}" for p in sorted(pathlib.Path(C.__file__).parent.glob("*.py")) if p.name != "common.py"
           for n, line in enumerate(p.read_text().splitlines(), 1) if "/compare/" in line]
assert by_hand == [], by_hand
# A base branch that's gone has nothing to compare against: GitHub says no.
FORK["gone"] = None
status, landed = at(clean(checks=docs("gone"), files=("README.md",), base="gone"))
assert not landed and JOBS == [] and "waiting for CI: flake for gone" in status, (status, JOBS)
del FORK["gone"]

# Retargeted onto `next` after its docs lane against main (or main's run run
# again since, or another PR's on this head): that run judged another diff.
WAIT = "waiting for CI: flake for next"
status, landed = at(clean(checks=docs(), files=("README.md",), base="next"))
assert not landed and JOBS == [] and WAIT in status, (status, JOBS)
# Even red, it's not this PR's red.
status, landed = at(clean(checks=docs(conclusion="FAILURE"), files=("README.md",), base="next"))
assert not landed and "CI is red" not in status and WAIT in status, status
# A retarget's run comes from flake-retarget.yml, flake.yml's twin: it's that
# workflow's run for `next`, so the push's for main no longer holds CI up.
def another_file(run, path, id):
    for c in run:
        c["checkSuite"]["workflowRun"].update(file={"path": path}, workflow={"id": id, "name": "flake"})
    return run


RETARGET = ".github/workflows/flake-retarget.yml"
status, _ = at(clean(checks=whole(base="main") + another_file(whole(base="next", suite="s2"), RETARGET, "twin"), base="next"))
assert "waiting for CI" not in status, status
# Another workflow that only shares its name is no such thing.
status, _ = at(clean(checks=whole(base="main") + another_file(whole(base="next", suite="s2"), ".github/workflows/ci.yml", "other"),
                     base="next"))
assert WAIT in status, status


def twin(run):
    return another_file(run, RETARGET, "twin")


# The two files answer for each other whichever ran last and however often the
# PR moved: there and back, a push after a retarget, and there and back again.
for why, checks, base in [
    ("back by the twin", whole("next") + twin(whole("main", "s2")), "main"),
    ("a push after a retarget", twin(whole("next")) + whole("main", "s2"), "main"),
    ("there and back", whole("main") + twin(whole("next", "s2")) + twin(whole("main", "s3")), "main"),
    ("a title edit after a retarget", whole("main") + twin(whole("next", "s2"))
     + twin([check("check / check", "SKIPPED", suite="s3")]), "next"),
]:
    status, _ = at(clean(checks=checks, base=base))
    assert "waiting for CI" not in status, (why, status)
# Its run stands in only once finished, and only for the base it judged: not a
# title edit's skipped run, one still going, or one for a base the PR has left.
for why, more in [
    ("a title edit", [check("check / check", "SKIPPED", suite="s2")]),
    ("still going", whole("next", "s2", done="IN_PROGRESS")),
    ("an old base", whole("other", "s2")),
]:
    status, _ = at(clean(checks=whole("main") + twin(more), base="next"))
    assert WAIT in status, (why, status)

# Nothing else stands in for a run of that workflow for `next` that has
# finished: another workflow's check run under the same job name, another
# App's, a run of it for yet another base, or one for `next` still going.
for why, more in [
    ("another workflow", [check("check / report", suite="s2", workflow="report")]),
    ("another App", [check("check / lane into next", suite="s2", app="some-app")]),
    ("another base", whole("other", "s2")),
    ("still going", whole("next", "s2", done="IN_PROGRESS")),
]:
    status, landed = at(clean(checks=docs() + more, files=("README.md",), base="next"))
    assert not landed and JOBS == [] and WAIT in status, (why, status, JOBS)
# The workflow ran for `next` and took the whole gate: that's the PR's CI,
# and the proofs run, whatever the old run said.
for old in ("SUCCESS", "FAILURE"):
    status, landed = at(clean(checks=docs(conclusion=old) + whole("next", "s2"), files=("README.md",), base="next"))
    assert not landed and PROVING in status and len(JOBS) == 1, (old, status, JOBS)
# ... and its whole gate red is the PR's red.
status, landed = at(clean(checks=docs() + whole("next", "s2", "FAILURE"), files=("README.md",), base="next"))
assert not landed and "CI is red" in status and "check / check" in status, status
# It took the docs lane for `next` too: that one counts.
status, landed = at(clean(checks=docs() + docs("next", suite="s2"), files=("README.md",), base="next"))
assert landed and JOBS == [] and "CI is green (docs only)" in status, (status, JOBS)

del S.CFG["repos"]["sam/r"]
fresh_pr_state()
print("docs lane: ok")
