"""A PR whose merge would change nothing on its base is closed as landed,
never merged: its head merged into the base's tip has the tip's own tree,
because another PR made the same change (two fixes for a red main, say).
adds_nothing reads that from the steward's mirror, fetched from the repo's
real remote here; handle() closes such a PR with a comment, and leaves a
dependency bot's to its bot."""

from harness import ALREADY, REAL, WROTE, clean, fresh_pr_state, merged, mkpr, pr_state, run, sh, stand_in, tmp

import reviewers as V

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
kept_origin, V.ORIGIN = V.ORIGIN, f"{origin}/{{}}.git"
adds_nothing = REAL["adds_nothing"]


def pr(n, rev):
    return {"number": n, "headRefOid": rev, "baseRefName": "main"}


# While main is red, every fix adds something.
for n, rev in ((1, fix_a), (2, fix_b), (3, more), (4, clash)):
    assert adds_nothing("sam/r", pr(n, rev)) is None, n
# The first fix lands (a squash of it): the second one now adds nothing,
# and says what main's tip has it. The others still change something,
# the one that clashes with it included (its merge conflicts).
sh("git", "checkout", "-q", "main", cwd=work)
landed = commit("fix a, squashed", **{"agents.nix": "packages.burrow-agent\n"})
sh("git", "push", "-q", remote, "main", cwd=work)
assert adds_nothing("sam/r", pr(2, fix_b)) == landed
assert adds_nothing("sam/r", pr(3, more)) is None
assert adds_nothing("sam/r", pr(4, clash)) is None
# Its merge is read onto main as it is: one that adds nothing there adds
# nothing after main moves on with something else too.
later = commit("later", **{"third.txt": "x\n"})
sh("git", "push", "-q", remote, "main", cwd=work)
assert adds_nothing("sam/r", pr(2, fix_b)) == later
# A head already in main (pushed there as it is) adds nothing too.
pull(5, landed)
assert adds_nothing("sam/r", pr(5, landed)) == later
# A PR that moved past the head the pass read: no word, and GitHub refuses
# that head's merge anyway.
pull(2, more)
assert adds_nothing("sam/r", pr(2, fix_b)) is None
pull(2, fix_b)
# git that can't fetch it raises: the steward can't tell, so it can't land.
V.ORIGIN = f"{tmp}/no-such-origin/{{}}.git"
try:
    adds_nothing("sam/r", pr(2, fix_b))
    raise AssertionError("a fetch that fails said something")
except RuntimeError as e:
    assert "git -C" in str(e), e
V.ORIGIN = kept_origin
print("adds nothing: ok")

# handle(): a settled PR whose merge would add nothing is closed, saying
# so, and never merged; its status says why.
fresh_pr_state()
ALREADY[5] = later
status, _ = run(clean(), 900)
assert not merged(), WROTE
assert "state=pending" in status and f"Already landed: main at {later[:7]} already has everything it changes; closing it" in status, status
closes = [x for x in WROTE if x.startswith("pr close 5 -R sam/r --comment ")]
assert len(closes) == 1 and f"Closed without merging: main at {later[:7]} already has everything it changes" in closes[0], WROTE
assert "strikes" not in pr_state() or not pr_state()["strikes"], pr_state()
# Behind a base whose rules want it up to date, it isn't brought up to date
# first: the merge of main into it would change nothing either.
fresh_pr_state()
status, _ = run({**clean(), "mergeStateStatus": "BEHIND"}, 900)
assert not any("/update-branch" in x for x in WROTE) and any(x.startswith("pr close 5") for x in WROTE), WROTE
# A dependency bot's is left to its bot, which closes its own: closed under
# it, it's an update the bot ignores from then on.
fresh_pr_state()
bump = {"branch": "dependabot/npm/foo-1", "title": "Bump foo from 1.4 to 1.5", "number": 13}
ALREADY[13] = later
status, _ = run(mkpr(**bump), 900)
assert not merged() and not any(x.startswith("pr close") for x in WROTE), WROTE
assert "Already landed" in status and "its bot closes it" in status, status
# One that adds something is merged as before.
ALREADY.clear()
fresh_pr_state()
run(clean(), 900)
assert merged() and not any(x.startswith("pr close") for x in WROTE), WROTE
print("landed: ok")
