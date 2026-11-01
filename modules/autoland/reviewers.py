"""The steward's reviewers: Codex (the codex CLI), an agent that reads a
snapshot of the PR and answers with its findings.

A pass queues the review (`queue`, burrow-upkeep's, which takes the
snapshot and writes the brief);
the reviewers' service (crates/burrow-review) runs the agents as a user of
their own, with no GitHub access and no secrets paths in what they read;
the next pass posts what came back (`post`, burrow-upkeep's), as the
steward's App and marked as that reviewer's, and reads each reviewer's
verdict back from those posts (`verdict`); and each pass sweeps away what
no open PR needs any more (`sweep`, burrow-upkeep's)."""

import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess

import common as C
from common import CFG, DRY, MIN, STATE, load, log, ts

NAMES = {"codex": "Codex"}
# Under STATE: what's asked for, what each snapshot keeps of its base (its
# NAME.json: the review rules, and the paths the reviewers weren't shown,
# which a judge looks at before the PR lands), what came back, and a clone
# per repo the snapshots are taken from.
QUEUE, SNAP, DONE, GIT = "review/queue", "review/snap", "review/done", "review/git"
# Each snapshot's checkout, the tree a reviewer reads: rewritten for every
# head and taken again from GitHub whenever it's missing, so it's a cache,
# kept out of STATE and whatever snapshots the machine keeps of its state
# (a big repo's runs to the best part of a gigabyte a head). Absolute, so
# os.path.join(STATE, TREES) is TREES.
TREES = CFG["snapDir"]
# Each reviewer's session for each open PR, which every look at it resumes.
# The reviewers' service owns what's in them; the sweep marks a closed PR's
# gone (burrow-upkeep's sweep.rs), and the service deletes it.
SESSIONS = "review/sessions"
OUT = "reviewers.json"  # a reviewer out of service, and until when
# How the reviewers (burrow-upkeep's brief.rs, handed it by `queue`) and a
# judge (tasks.py) weigh the reasons a PR's description gives where the
# house rules ask for one. That the line is there is CI's to check;
# whether its reason holds up takes reading the code.
REASONS = ("Where the house rules ask a PR for a line in its description giving a reason (one starting `Why not Rust:` "
           "for new code that isn't Rust, say), judge the reason against the rule it answers and against the code, not only "
           "that it's there. Rust is the default, and a coin toss goes to Rust: a reason that only says it was quicker, that "
           "it's small, that the code around it is in that language already, or that the code itself contradicts (a "
           "\"throwaway\" script a service runs, \"glue\" with logic of its own) doesn't hold.")


def on(repo):
    """The reviewers this repo has, in a fixed order."""
    rc = C.repo_cfg(repo)
    return [r for r in NAMES if CFG["reviewers"][r]["enable"] and rc[r]]


def out(r):
    """Why the reviewer is out of service (usage spent, not logged in), or
    None. Out, it isn't asked and isn't waited for."""
    o = load(OUT, {}).get(r)
    return o["why"] if o and C.NOW < o["until"] else None


def review_re(r):
    return re.compile(rf"<!-- {r}-review:v1 (\{{.*?\}}) -->")


def finding(r):
    return f"<!-- {r}-finding -->"


def described(pr, seen):
    """Whether a review that was shown the description `seen` (its
    fingerprint) was shown the PR's as it is now. The reasons a description
    gives are part of what a reviewer judges, so a review of another one
    doesn't cover the PR: a `Why not Rust:` line written in after a clean
    review (CI wanted one) has had nobody weigh it."""
    return seen == C.fingerprint(pr["body"])


def verdict(pr, head, r):
    """What the reviewer last said about this exact head on the PR's base,
    shown its description as it is now (clean, findings or none), when, and
    how many of its reviews on the PR had findings. Only reviews the steward
    posted count: anyone can write the marker."""
    pat = review_re(r)
    v, at, rounds = "none", 0, 0
    for x in pr["reviews"]["nodes"]:
        m = pat.search(x["body"] or "")
        if not m or not C.mine(x["author"]):
            continue
        try:
            said = json.loads(m.group(1))
            found = int(said.get("findings", 0)) > 0
        except (ValueError, TypeError, AttributeError):
            continue
        rounds += found
        # Retargeted, the PR is another change on the same head: what was
        # said about it on its old base doesn't count.
        if (said.get("commit_sha") == head and said.get("base") == pr["baseRefName"] and described(pr, said.get("description"))
                and (x["commit"] or {}).get("oid") == head and ts(x["submittedAt"]) >= at):
            v, at = ("findings" if found else "clean"), ts(x["submittedAt"])
    return v, at, rounds


# ---------------------------------------------------------------- the snapshot

def git(*args, cwd=None, input=None, env=None):
    p = subprocess.run(["git", *args], cwd=cwd, input=input, env=env, capture_output=True, check=False)
    if p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:2])}: {p.stderr.decode(errors='replace').strip()[:300]}")
    return p.stdout


def fetch(repo, *refs):
    """The steward's bare clone of the repo, which only it reads, with refs
    fetched into it from GitHub (burrow-upkeep's `fetch`): what the
    snapshots are taken from, and what a proof's builds are worked out from
    (proofs.py). As long as git takes, as it always has."""
    out = C.upkeep("fetch", repo, input=json.dumps(refs), bounded=False)
    if out["mirror"] is None:
        raise RuntimeError(out["why"])
    return out["mirror"]


def snap_name(key, head, base):
    """One snapshot a head on a base: retargeted, a PR is another change."""
    return f"{key[:-5]}-{head[:12]}-{hashlib.sha1(base.encode()).hexdigest()[:8]}"


def remove(p):
    """p gone, whether this takes it or something else (the reviewers'
    service, done with a queued review) got there first."""
    try:
        if os.path.isdir(p) and not os.path.islink(p):
            shutil.rmtree(p)
        else:
            os.remove(p)
    except FileNotFoundError:
        pass


def item_name(key, r, head, base):
    return f"{r}--{snap_name(key, head, base)}"


def queue(repo, pr, key, r):
    """Queue the reviewer on the head's snapshot, briefed with the PR's
    description as it is now: one edited to answer a finding is what the
    next look at the same head reads, and its fingerprint goes back with the
    answer (`described`). burrow-upkeep's `queue` takes the snapshot
    (snapshot.rs), writes the brief (brief.rs) with the reviewer's earlier
    findings, and leaves the entry the reviewers' service takes. In line by
    when the PR was opened, so one pushed again keeps its place. As long as
    git takes; raises if it can't be queued (the head moved on, GitHub's
    unreachable)."""
    head, base = pr["headRefOid"], pr["baseRefName"]
    out = C.upkeep("queue", repo, bounded=False, input=json.dumps({
        "number": pr["number"], "head": head, "base": base, "title": pr["title"], "quoted": C.quote(pr["body"]),
        "description": C.fingerprint(pr["body"]), "opened": ts(pr["createdAt"]), "reviewer": r,
        "name": snap_name(key, head, base), "reasons": REASONS}))
    if out["queued"] is None:
        raise RuntimeError(out["why"])


def ask(repo, pr, key, r, rs, after):
    """Queue the reviewer on the PR's head, unless it's already on its way.
    Its looks at one head are bounded by `rounds`: each one with findings
    counts as a round. rs is the reviewer's state on the PR, which keeps
    its tries; after books the PR's next pass (step.py's)."""
    head = pr["headRefOid"]
    tag = f"{repo}#{pr['number']}"
    if rs.get("queued"):
        return
    if DRY:
        log(f"  would: ask {NAMES[r]} to review {tag} at {head[:7]}")
        return
    if rs.get("fails", 0) >= 3 and C.NOW - rs.get("failedAt", 0) > 3600:
        rs["fails"] = 0  # an outage passes: three more tries an hour on
    if rs.get("fails", 0) >= 3:
        after(rs.get("failedAt", 0), 60)  # ends: listed (stuck, step.py), three more tries an hour on
        return
    if rs.get("fails") and C.NOW - rs["failedAt"] < 5 * MIN:
        # Tries five minutes apart, however many events come meanwhile:
        # a hiccup gets time to pass before all three are spent.
        after(rs["failedAt"], 5)  # ends: three tries, then hourly, and listed
        return
    try:
        queue(repo, pr, key, r)
    except Exception as e:  # the snapshot: GitHub, git, the disk
        rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
        log(f"{tag}: couldn't queue {NAMES[r]} on {head[:7]} ({rs['fails']}): {str(e)[:200]}")
        after(C.NOW, 5)  # ends: three tries, then hourly, and listed
        return
    log(f"{tag}: queued a {NAMES[r]} review of {head[:7]}")
    rs["queued"] = C.NOW


def unseen(key, head, base):
    """What the head changes that its snapshot hid from the reviewers
    (secrets paths, and plain ones a secret was moved to). [] until the
    snapshot's taken."""
    return (load(f"{SNAP}/{snap_name(key, head, base)}.json", None) or {}).get("secret", [])


def drop(name):
    """A review's queue entry and answer."""
    for p in (f"{QUEUE}/{name}.json", f"{DONE}/{name}.json"):
        try:
            os.remove(os.path.join(STATE, p))
        except OSError:
            pass


def mine(d, key, r):
    """The names of the reviewer's entries in `d` (the queue, the answers),
    for any head, for the PR whose state file is `key`: parsed, not matched
    by prefix, since sam/r-5's names start with sam/r PR 5's. [] if there's
    no such directory (a status run that couldn't make it)."""
    try:
        names = [x.removesuffix(".json") for x in os.listdir(os.path.join(STATE, d))]
    except OSError:
        return []
    return sorted(n for n in names if n.partition("--")[0] == r and n.partition("--")[2].rsplit("-", 2)[0] == key[:-5])


def answered(key, r):
    """The names of the reviewer's answers, for any head, to the PR."""
    return mine(DONE, key, r)


def in_queue(key, r, base):
    """The names of the reviewer's looks at the PR on `base`, for any head,
    still in the queue, begun or not."""
    on = snap_name(key, "", base).rsplit("-", 1)[1]
    return [name for name in mine(QUEUE, key, r) if name.rsplit("-", 1)[1] == on]


def under_way(key, r, base):
    """The names of the reviewer's looks at the PR on `base`, for any head,
    that the reviewers' service is on now (on another base, the PR was
    another change). Those it hasn't begun are taken off the queue
    (`taken`), so none begins after this."""
    on = snap_name(key, "", base).rsplit("-", 1)[1]
    return [name for name in mine(QUEUE, key, r)
            if name.rsplit("-", 1)[1] == on and not taken(os.path.join(STATE, QUEUE, name + ".json"))]


def taken(path):
    """Takes the queued review at path off the queue unless the reviewers'
    service is on it (it holds the entry's lock while it is: burrow-review's
    `review`). False if it is, or if it can't be opened to tell."""
    try:
        f = open(path)
    except FileNotFoundError:
        return True
    except OSError:
        return False
    with f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        remove(path)  # under the lock: the service, waiting on it, sees it gone
        return True


def sweep(repo, live):
    """What the reviewers leave of the repo's PRs, swept once nothing needs
    it (burrow-upkeep's `sweep`, crates/burrow-upkeep/src/sweep.rs): the
    snapshots, queue entries and answers of heads none of its open PRs has
    any more (`live`: the snapshot names of the heads they have), and the
    sessions of its PRs that closed or were retargeted, marked gone for the
    reviewers' service to delete. A look the service is on stays, with what
    it reads, until it ends. Returns what it left of the heads it swept:
    their looks under way, and the sessions it couldn't mark gone. Raises
    when it couldn't sweep. As long as deleting takes, as it always has,
    and as nobody: it asks GitHub nothing."""
    out = C.upkeep("sweep", repo, input=json.dumps(sorted(live)), as_app=False, bounded=False)
    if out["left"] is None:
        raise RuntimeError(out["why"])
    return out["left"]


# ---------------------------------------------------------------- posting

def post(repo, pr, r, answer, description):
    """The reviewer's answer as one review on the head, marked with the
    description it was shown (its fingerprint, or None if that's unknown),
    by burrow-upkeep (crates/burrow-upkeep/src/post.rs). Returns "clean" or
    "findings". Raises ValueError if the answer isn't a review, RuntimeError
    if GitHub didn't take it (the same answer can go next pass), and
    C.Unavailable if GitHub didn't answer."""
    out = C.upkeep("post", repo, input=json.dumps({
        "number": pr["number"], "head": pr["headRefOid"], "base": pr["baseRefName"], "reviewer": r, "name": NAMES[r],
        "answer": answer, "description": description}))
    if out["outcome"] in ("clean", "findings"):
        return out["outcome"]
    raise {"notAReview": ValueError, "refused": RuntimeError, "unanswered": C.Unavailable}[out["outcome"]](out["why"])
