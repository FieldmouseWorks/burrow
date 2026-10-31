"""autoland-steward: every PR in every repo the owner has, from ready to merged.

A full pass (burrow-events runs one when it starts, and hourly, for what no
event reports):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md (repos.py).
  2. For each open PR that isn't a draft, work out where it stands and do
     the next thing (step.py): queue a review of its head from each
     reviewer (Codex, reviewers.py), post what they found, hand open
     findings, red CI or a conflict with its base to a Claude job, have a
     Claude job run the checks only a machine of the owner's runs
     (proofJobs) or judge a PR it won't land on its own word (tasks.py says
     what each job is told), post the `review-settled` commit status and
     merge it, or list it for the owner.
  3. Delete the branches whose tip is already in the default branch or a
     closed PR's head, and archive the stale unmerged ones under
     refs/archive before deleting them too, as a push or a PR closing
     changes them or the wait they were given ends.
  4. Read each repo's Renovate dashboard: what sits on it too long, short
     of a PR, goes to a Claude job (dashboard.py); and its quarantined
     flaky tests, each to a Claude job that fixes it (flakes.py).
  5. Keep one issue up to date with everything waiting for the owner, and
     one with what the pipeline is stuck on, for a Claude job (repos.py).

`autoland-steward kick OWNER/REPO` is a pass over that repo alone (2, 4
and 5), run by its autoland@ unit (only that holds the App's key), the
one burrow-events starts within seconds of what changes it: a webhook
delivery, a Claude job's turn ending, a reviewer's answer coming
back, or a wait the last pass booked ending (common.wake).

Everything talks to GitHub as the steward's own GitHub App (common.py), so
its statuses, merges and the reviewers' reviews are posted as the App, and
nobody else's count as its word. Its Claude jobs work as the machine user.
Settings are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is
in config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward review` works through the reviews the passes queued
(review_worker.py), as the reviewers' own user. A careful look takes an
agent many minutes, so it runs as its own service and no pass waits for it.

In a repo with `train` on, a settled PR goes into its merge train instead of
being merged, and a pass ends with a step of the train (train.py). `cn`, which
runs the train, gets the App's tokens from `autoland-steward token
OWNER/REPO`, printed on stdout for it alone.
"""

import fcntl
import os
import sys

import common as C
import dashboard as D
import flakes as F
import proofs as P
import repos as R
import reviewers as V
import step
import train as T
import waits
from common import CFG, DRY, MIN, STATE, load, log, save
from prs import open_prs


# ---------------------------------------------------------------- main

def token(repo):
    """The App's token for repo's owner, for cn (train.py), which runs it
    inside a pass: so it takes no lock. Exits 75 when GitHub can't be asked
    just now, as cn does."""
    try:
        print(C.token(repo.split("/")[0].lower()))
    except C.Unavailable as e:
        print(f"no token for {repo}: {e}", file=sys.stderr)
        sys.exit(75)
    except (RuntimeError, OSError) as e:
        print(f"no token for {repo}: {e}", file=sys.stderr)
        sys.exit(1)


def main():
    if sys.argv[1:] == ["review"]:
        import review_worker
        review_worker.work()
        return
    if sys.argv[1:2] == ["token"] and len(sys.argv) == 3:
        token(sys.argv[2])
        return
    try:
        for d in ("", V.QUEUE, V.SNAP, V.DONE, V.SESSIONS, V.GIT):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
    except OSError:
        if not DRY:
            raise
    # `kick REPO...`: just those repos' PRs, now (burrow-events, on what
    # changed on GitHub or here, or a wait that has ended). The full pass,
    # when burrow-events starts and hourly, does the rest: the settings, new
    # repos' baseline jobs and another try at archiving a branch.
    kick = [k.lower() for k in sys.argv[2:]] if sys.argv[1:2] == ["kick"] else None
    if not DRY:
        # One pass at a time, full or kicked: two would act on the same PRs.
        lock = open(os.path.join(STATE, "pass.lock"), "w")
        fcntl.flock(lock, fcntl.LOCK_EX)
    try:
        run_pass(kick)
    finally:
        if C.LIMITED and C.LIMITED[0] > C.NOW:
            C.after_limit(kick)


def run_pass(kick):
    waits.begin()
    repos = R.discover()
    if kick is not None:
        repos = [r for r in repos if r["repo"].lower() in kick]  # GitHub's names are case-blind
    # Repos whose baseline job has started. A repo not in it gets one, a few
    # per pass so the first pass on a machine doesn't start them all at once.
    known = set(load("known.json", []))
    # Ones whose job didn't start: tried again a day later, so they don't
    # take every pass's slots.
    failed = load("baseline-failed.json", {})
    started = 0
    # What waits for the owner, by repo: a kicked pass replaces its repos'.
    digest = {}
    read, live = set(), set()  # the repos whose PRs it read, and their heads' snapshots
    covered = []
    for r in repos:
        repo = r["repo"]
        digest[repo] = mine = []
        with C.acting_for(repo):
            try:
                new = kick is None and R.settle_repo(r, known)
                if (new and CFG["baselineJobs"] and started < CFG["baselinePerPass"]
                        and C.NOW - failed.get(repo, 0) > 86400):
                    jid = C.job(repo, CFG["baselineTask"].replace("REPO", repo), clone=True)
                    started += 1
                    if jid:
                        log(f"{repo}: baseline job {jid}")
                        known.add(repo)
                        failed.pop(repo, None)
                    else:
                        log(f"{repo}: baseline job didn't start (no checkout, or proj refused); trying again tomorrow")
                        failed[repo] = C.NOW
                elif new and not CFG["baselineJobs"]:
                    known.add(repo)
                owner, name = repo.split("/")
                info, prs = open_prs(owner, name)
                # The PRs labelled to go first in the proof line. One whose
                # labels can't be read isn't, and fails in its own step below:
                # one bad PR mustn't stop the rest.
                first = {pr["number"] for pr in prs if CFG["labels"]["first"] in
                         {x.get("name") for x in pr.get("labels", {}).get("nodes", ())}}
                P.begin(repo, (pr["number"] for pr in prs), first)
                T.begin(repo)
                read.add(repo.replace("/", "_"))
                waits.read(repo, prs)
                # First, so a fix PR it made obsolete isn't merged by this same pass.
                obsolete = R.watch_default(r, info, prs, mine) or set()
                # Every open PR's branches, head and base, drafts and forks' PRs
                # included: the branch sweep leaves them all (deleting a base
                # closes the PRs on it).
                in_use = set()
                seen_to = True
                # Obsolete fixes first: their holds take back a queued merge
                # before anything else this pass waits on.
                # Then in the proof line's order, so proofSlots go to PRs in it.
                for pr in sorted(prs, key=lambda pr: (pr["number"] not in obsolete, P.place(repo, pr["number"]))):
                    in_use.update((pr["headRefName"], pr["baseRefName"]))
                    live.add(V.snap_name(f"pr-{repo.replace('/', '_')}-{pr['number']}.json", pr["headRefOid"], pr["baseRefName"]))
                    try:
                        step.handle(repo, pr, info, mine, pr["number"] in obsolete)
                    except Exception as e:  # one bad PR mustn't stop the rest
                        log(f"{repo}#{pr['number']}: {e}")
                        waits.failed(repo, pr)
                        seen_to = False
                        if T.seat(repo, pr["number"]) is not None:
                            # It may not be settled any more, and still be in.
                            T.stop(repo, f"#{pr['number']} wasn't seen to")
                if seen_to:
                    covered.append(r)
                else:
                    C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the PR it failed on
                if T.on(repo):
                    # After its PRs, so the ones settled this pass are in it.
                    try:
                        T.step(repo)
                    except C.Unavailable as e:
                        log(f"{repo}: its train: {e}")
                        C.wake(repo, C.NOW + 5 * MIN)  # ends: the next pass steps it again
                    except Exception as e:  # its own: the repo's PRs were seen to all the same
                        log(f"{repo}: its train: {e}")
                        waits.failed(repo)
                        C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo
                if CFG["cleanBranches"]:
                    R.watch_branches(r, in_use, mine, full=kick is None)
            except Exception as e:
                log(f"{repo}: {e}")
                waits.failed(repo)
                C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo; GitHub, often
            if CFG["dashboard"]["enable"]:
                try:
                    D.watch(repo, mine)
                except Exception as e:  # its own: the repo's PRs were seen to all the same
                    log(f"{repo}: its dashboard: {e}")
                    waits.failed(repo)
                    C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo
            if CFG["flaky"]["enable"]:
                try:
                    F.watch(repo, mine)
                except Exception as e:  # likewise
                    log(f"{repo}: its flaky tests: {e}")
                    waits.failed(repo)
                    C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo
    # This pass's wake-ups and owner's lines replace the ones it had for the
    # repos it saw; a full pass's, all of them.
    seen = {r["repo"] for r in repos}
    wakes = {k: v for k, v in load("wake.json", {}).items() if v > C.NOW and k not in seen}
    save("wake.json", {**wakes, **C.WAKE})
    if kick is not None:
        digest = {**{k: v for k, v in load("digest-repos.json", {}).items() if k not in seen}, **digest}
    save("digest-repos.json", digest)
    waits.publish(seen, digest, kick is None)
    if kick is None:
        # For renovate-sweep: it lists the ok majors of the repos this pass
        # didn't see to (one whose pass failed among them), and tells its
        # jobs how every repo's proofs run.
        save("repos.json", C.coverage(repos, covered))
        save("known.json", sorted(known))
        save("baseline-failed.json", failed)
    if not DRY:
        V.sweep(read, live)
    if kick is None:
        R.unhook_gone(repos + R.hook_projects(repos))
    # Last: GitHub failing it leaves the pass's state saved. A line more than
    # one repo has (a reviewer out of service) is listed once. The stalls the
    # watchdog took past a Claude job go on the stuck issue with the rest.
    R.publish(list(dict.fromkeys(tuple(x) for repo in sorted(digest) for x in digest[repo])) + waits.escalated())


if __name__ == "__main__":
    main()
