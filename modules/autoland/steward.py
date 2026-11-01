"""autoland-steward: every PR in every repo the owner has, from ready to merged.

A full pass (burrow-events runs one when it starts, and hourly, for what no
event reports):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included (one with no commit yet too, which gets
     only this step, all but the ruleset, until its first push). Bring
     each one's labels and settings to the baseline (auto-merge,
     delete-branch-on-merge, squash, secret scanning, the `baseline`
     ruleset: burrow-upkeep's `settle`) and set its hook; a repo seen for
     the first time also gets a Claude job that brings its files to
     docs/repo-baseline.md (repos.py). So does
     one made under the name of a repo GitHub deleted, once the deleted
     one's Claude jobs are stopped, its clones moved aside and what the
     steward kept about it forgotten: before anything else acts on it.
  2. For each open PR that isn't a draft, work out where it stands and do
     the next thing (step.py): queue a review of its head from each
     reviewer (Codex, reviewers.py), post what they found, hand open
     findings, red CI or a conflict with its base to a Claude job, build
     the checks only a machine of the owner's runs that are a build
     (proofBuilds, proofs.py) and have a Claude job run the rest
     (proofJobs), or judge a PR it won't land on its own word (tasks.py says
     what each job is told), post the `review-settled` commit status and
     merge it, or list it for the owner.
  3. Delete the branches whose tip is already in the default branch or a
     closed PR's head, and archive the stale unmerged ones under
     refs/archive before deleting them too, as a push, a PR closing or
     the default moving changes them or the wait they were given ends
     (burrow-upkeep's `branches`).
  4. Read each repo's Renovate dashboard: what sits on it too long, short
     of a PR, goes to a Claude job; and its quarantined flaky tests, each
     to a Claude job that fixes it (burrow-upkeep, `upkeep` below).
  5. Keep one issue up to date with everything waiting for the owner, and
     one with what the pipeline is stuck on, for a Claude job (repos.py).

`autoland-steward kick OWNER/REPO` is a pass over that repo alone (1 to
5), run by its autoland@ unit (only that holds the App's key), the
one burrow-events starts within seconds of what changes it: a webhook
delivery (the App's own, for a repo just made), a Claude job's turn
ending, a reviewer's answer coming back, or a wait the last pass booked
ending (common.wake). Its settings cost nothing unless they're due, so a
repo is set up from the first event about it.

Everything talks to GitHub as the steward's own GitHub App (common.py), so
its statuses, merges and the reviewers' reviews are posted as the App, and
nobody else's count as its word. Its Claude jobs work as the machine user.
Settings are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is
in config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward build ID` is a proof build, run by the unit a pass starts
for it (proofs.py): the steward builds a proof check that's only a build
itself, with no Claude job.

The reviews the passes queue are worked through by the reviewers' own
service (crates/burrow-review, `autoland-review`), as a user of their own. A
careful look takes an agent many minutes, so no pass waits for it.

In a repo with `train` on, a settled PR goes into its merge train instead of
being merged, and a pass ends with a step of the train (train.py). `cn`, which
runs the train, gets the App's tokens from `autoland-steward token
OWNER/REPO`, printed on stdout for it alone.
"""

import fcntl
import json
import os
import sys

import common as C
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
    if sys.argv[1:2] == ["token"] and len(sys.argv) == 3:
        token(sys.argv[2])
        return
    if sys.argv[1:2] == ["build"] and len(sys.argv) == 3:
        # A proof build's unit (proofs.build): no pass, so no lock.
        P.run(sys.argv[2])
        return
    if sys.argv[1:2] == ["realise"] and len(sys.argv) == 3:
        # Its derivations built, in its heavy slot (proofs.run).
        sys.exit(P.realise(sys.argv[2]))
    try:
        for d in ("", V.QUEUE, V.SNAP, V.TREES, V.DONE, V.SESSIONS, V.GIT):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
    except OSError:
        if not DRY:
            raise
    # `kick REPO...`: just those repos, now (burrow-events, on what changed
    # on GitHub or here, or a wait that has ended). The full pass, when
    # burrow-events starts and hourly, does the rest: the App's webhook,
    # the hooks of repos it no longer sees to, another try at what GitHub
    # said no to about a branch (its archive, a read its judgment needed),
    # and whatever no event reported.
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


# How many repos this pass has searched for a dashboard (burrow-upkeep's
# FINDS says how many it may).
SEARCHED = [0]


def failing(repo, why):
    """What of burrow-upkeep's look failed: logged, and the stall watchdog
    watches the repo."""
    log(f"{repo}: {why}")
    waits.failed(repo)
    C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo


def told(repo, out, mine):
    """What a look of burrow-upkeep's says: what it lists goes in mine, the
    pass it books is booked, and what failed is `failing`."""
    mine.extend(tuple(x) for x in out["stuck"])
    if out["wake"]:
        C.wake(repo, out["wake"])  # ends: as each of burrow-upkeep's own says (its `// ends:`)
    for why in out["failed"]:
        failing(repo, why)


def branches(repo, default, in_use, mine, full):
    """The branches nothing needs any more, which burrow-upkeep deletes,
    leaving `in_use`, every open PR's head and base: told. A full pass's
    also tries again what GitHub said no to. It raises when it fails, as
    the repo's other steps do."""
    told(repo, C.upkeep("branches", repo, "--default", default, *(["--full"] if full else []),
                        input=json.dumps(sorted(in_use))), mine)


def upkeep(repo, mine):
    """The repo's Renovate dashboard and quarantined flaky tests, which
    burrow-upkeep looks at: told, and the Claude jobs it asks for are
    started here, each under the key it gives, by which its next look finds
    the job (whatever became of its id here). Whatever fails is
    `failing`; the pass goes on."""
    try:
        out = C.upkeep("watch", repo, "--searched", str(SEARCHED[0]))
    except Exception as e:
        failing(repo, f"its upkeep: {e}")
        return
    told(repo, out, mine)
    SEARCHED[0] += out["searched"]
    for j in out["jobs"]:
        try:
            jid = C.job(repo, j["task"], clone=True, pipeline=j["pipeline"], timeout=j["timeout"], key=j["key"])
        except Exception as e:  # its own: the other part's job, and the pass, go on
            failing(repo, f"its {j['part']} job didn't start: {e}")
            mine.extend(tuple(x) for x in j["unstarted"])
            continue
        if not jid:
            log(f"{repo}: couldn't start its {j['part']} job (no checkout, or proj refused)")
            mine.extend(tuple(x) for x in j["unstarted"])
            if not C.taken(repo):  # no use looking again until that folder moves: a full pass will
                C.wake(repo, C.NOW + 5 * MIN)  # ends: a job starting; until then each one is listed
            continue
        C.wake(repo, j["wake"])  # ends: the job's own end, or this look (its `// ends:`)


def retire_deleted(known, failed, digest):
    """The deleted repos whose clones follow_renames wouldn't adopt, retired
    as restart retires one. Kept until that's all done, however far a try got
    (the clones may be moved by then, and only its jobs left); each step is as
    safe twice as once. One that won't goes on the stuck issue."""
    pending = load(C.RETIRING_FILE, {})
    for k, x in list(pending.items()):
        try:
            if any(have == k for have, _ in C.kept_state(x["name"])):  # gone: a try got to its last step
                R.reset({"repo": x["name"]}, k, known, failed)
            del pending[k]
            save(C.RETIRING_FILE, pending)
        except Exception as e:  # its own: the next full pass tries again, and its name's taker gets no job meanwhile
            log(f"{x['name']}: retiring it: {e}")
            digest[x["name"]] = [(C.RETIRING, x["name"], f"{x['for']} took on its name, and gets no job until this is done (journalctl -u autoland)")]


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
    if kick is None and not DRY:
        C.follow_renames({r["repo"].lower() for r in repos})
        retire_deleted(known, failed, digest)
    read, live = set(), set()  # the repos whose PRs it read, and their heads' snapshots
    covered = []
    for r in repos:
        repo = r["repo"]
        digest[repo] = mine = []
        with C.acting_for(repo):
            try:
                # First: nothing of a deleted repo's may act on the one that
                # took its name, so until that's done nothing else is.
                R.restart(r, known, failed)
            except Exception as e:
                log(f"{repo}: starting it over as new: {e}")
                waits.failed(repo)
                C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo
                continue
            try:
                new = R.settle_repo(r, known)
                if r["default"] is None:
                    # No commit yet: its settings and hook are set, so its
                    # first push is heard, and that push's pass does the rest.
                    continue
                if (new and CFG["baselineJobs"] and started < CFG["baselinePerPass"]
                        and C.NOW - failed.get(repo, 0) > 86400 and not C.taken(repo)):
                    jid = C.job(repo, CFG["baselineTask"].replace("REPO", repo), clone=True)
                    started += 1
                    if jid:
                        log(f"{repo}: baseline job {jid}")
                        known.add(repo)
                        failed.pop(repo, None)
                    elif not C.taken(repo):  # which goes on the stuck issue, below
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
                # First, so a red-branch fix it holds isn't merged by this same pass.
                held = R.watch_default(r, info, prs, mine) or {}
                # Every open PR's branches, head and base, drafts and forks' PRs
                # included: the branch watch leaves them all (deleting a base
                # closes the PRs on it).
                in_use = set()
                seen_to = True
                # Held fixes first: their holds take back a queued merge
                # before anything else this pass waits on.
                # Then in the proof line's order, so proofSlots go to PRs in it.
                for pr in sorted(prs, key=lambda pr: (pr["number"] not in held, P.place(repo, pr["number"]))):
                    in_use.update((pr["headRefName"], pr["baseRefName"]))
                    live.add(V.snap_name(f"pr-{repo.replace('/', '_')}-{pr['number']}.json", pr["headRefOid"], pr["baseRefName"]))
                    try:
                        step.handle(repo, pr, info, mine, held.get(pr["number"]))
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
                branches(repo, r["default"], in_use, mine, full=kick is None)
            except Exception as e:
                log(f"{repo}: {e}")
                waits.failed(repo)
                C.wake(repo, C.NOW + 5 * MIN)  # ends: waits.failed watches the repo; GitHub, often
            upkeep(repo, mine)
            if had := C.taken(repo):
                mine.append(C.taken_line(repo, had))
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
        R.hooks(repos)
    # Last: GitHub failing it leaves the pass's state saved. A line more than
    # one repo has (a reviewer out of service) is listed once. The stalls the
    # watchdog took past a Claude job go on the stuck issue with the rest.
    R.publish(list(dict.fromkeys(tuple(x) for repo in sorted(digest) for x in digest[repo])) + waits.escalated())


if __name__ == "__main__":
    main()
