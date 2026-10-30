"""The docs lane: a head burrow's shared CI took in its docs lane for the PR's
base (every path it changes since it left that base is the repo's
documentation, nothing its flake builds or tests reads) gets none of the
repo's proofs, and lands on its reviewers and CI alone, saying so. Only
GitHub Actions' passed `docs only into BASE` check run counts, and only on a
PR that changes nothing under .github/, renames out of it included, with
every file it changes read: there the workflow that judged it is the base
branch's. A run that took the lane against another base (the PR retargeted
since, an old run run again, another PR's on the same head) judged another
diff: none of its check runs is this PR's CI, and until the workflow runs
for this base, CI is pending. Anything short of that runs the proofs as
before."""

from harness import JOBS, PR_DIFF, S, clean, fresh_pr_state, merged, run

S.CFG["repos"]["sam/r"] = {"proofJobs": {"agents": "Run `nix build .#checks.agents` and post the agents status."}}
PROVING = "a Claude job is running agents"


def check(name, conclusion="SUCCESS", status="COMPLETED", app="github-actions", suite="s1"):
    return {"__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion,
            "checkSuite": {"id": suite, "app": {"slug": app}}}


def docs(base="main", suite="s1", **kw):
    """One run of the shared CI that took the docs lane against base."""
    return [check("check / lane", suite=suite), check("check / check", "SKIPPED", suite=suite),
            check(f"check / docs only into {base}", suite=suite, **kw)]


def whole(suite="s1", conclusion="SUCCESS"):
    """One that took the whole gate. The docs lane's job, skipped, may keep
    its name unfilled."""
    return [check("check / lane", suite=suite), check("check / check", conclusion, suite=suite),
            check("check / docs only into ${{ github.event.pull_request.base.ref }}", "SKIPPED", suite=suite)]


def at(pr, t=700):
    """One pass on a fresh PR: its status, and whether it merged."""
    fresh_pr_state()
    status, _ = run(pr, t)
    return status, merged()


# Documentation only, as CI found against the PR's base: no proof, and it
# lands saying why.
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

# Retargeted onto `next` after its docs lane against main (or main's run run
# again since, or another PR's on this head): that run judged another diff.
# Nothing has run for `next` yet, so CI is pending, and no proof starts.
status, landed = at(clean(checks=docs(), files=("README.md",), base="next"))
assert not landed and JOBS == [] and "waiting for CI: check against next, not main" in status, (status, JOBS)
# Even red, it's not this PR's red.
status, landed = at(clean(checks=docs(conclusion="FAILURE"), files=("README.md",), base="next"))
assert not landed and "CI is red" not in status and "check against next, not main" in status, status
# The workflow ran for `next` and took the whole gate: that's the PR's CI,
# and the proofs run.
for old in ("SUCCESS", "FAILURE"):
    status, landed = at(clean(checks=docs(conclusion=old) + whole("s2"), files=("README.md",), base="next"))
    assert not landed and PROVING in status and len(JOBS) == 1, (old, status, JOBS)
# ... and its whole gate red is the PR's red.
status, landed = at(clean(checks=docs() + whole("s2", "FAILURE"), files=("README.md",), base="next"))
assert not landed and "CI is red" in status and "check / check" in status, status
# It took the docs lane for `next` too: that one counts.
status, landed = at(clean(checks=docs() + docs("next", "s2"), files=("README.md",), base="next"))
assert landed and JOBS == [] and "CI is green (docs only)" in status, (status, JOBS)

del S.CFG["repos"]["sam/r"]
fresh_pr_state()
print("docs lane: ok")
