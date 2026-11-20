"""Who lands each repo's PRs: the steward or conaryd (Conary#313). The
landing record says, read through the admission decision
(admission.landing), and burrow-landing alone writes it, moving a repo from
one to the other only once the one letting go has nothing left at work
there (burrow-settings' landing.rs).

Where the steward doesn't land a repo, its pass sees to the repo's settings,
hook and upkeep as ever, and to none of its PRs: no step, no job, no proof,
no merge, no train, no branch deleted by what its PRs use, no wait watched
and no wake-up booked for them. Every landing write is gated besides
(admission.held), so nothing that started before the record changed lands
after it.

While the steward lets a repo go to conaryd (`owner` steward, `to` conaryd)
it vacates: every Claude job and proof build its PRs' states name that's
still at work is stopped, and once none is, it says so in its release
marker, landing-released.json in its state dir ({repo: the record's
generation}), which is what burrow-landing waits for before conaryd takes
the repo. One proj won't stop yet is tried again a few minutes on."""

import json
import os

import common as C
from common import CFG, DRY, MIN, log

MARKER = "landing-released.json"


def prs_states(repo):
    """The steward's state of each of the repo's PRs it has one of, by file
    name (step_context's pr-OWNER_NAME-N.json), its name case-blind."""
    prefix = f"pr-{repo.replace('/', '_')}-".lower()
    try:
        names = os.listdir(CFG["stateDir"])
    except FileNotFoundError:
        return {}
    found = {}
    for name in names:
        low = name.lower()
        if low.startswith(prefix) and low.endswith(".json") and low[len(prefix):-5].isdigit():
            found[name] = C.load(name, {}, live=True)
    return found


def jobs(st):
    """Every job and proof build a PR's state names: each kind's (KINDJob),
    the proofs it started, what it's stopping, the fix jobs that may still
    push to its branch, and the build of what its proof is built from."""
    named = [v for k, v in st.items() if k.endswith("Job") and isinstance(v, str)]
    named += st.get("proofsLive", []) + st.get("stopping", []) + list((st.get("answering") or {}).keys())
    if (pre := st.get("proofInputs") or {}).get("job"):
        named.append(pre["job"])
    return list(dict.fromkeys(named))


def alive(jid):
    """Whether the job may still do something: at work (common.at_work), or
    parked by a pause, which would carry on once it's lifted."""
    snapshot = C.lifecycle(jid)
    return snapshot[0] == "parked" or C.at_work(jid, snapshot=snapshot)


def vacate(repo, generation):
    """Stop the steward's work in a repo it's letting go of. True once none
    is left, with its release marker written for this handover."""
    left = []
    for name, st in prs_states(repo).items():
        for jid in jobs(st):
            if not alive(jid):
                continue
            if C.stop_job(jid):
                log(f"{repo}: stopped {jid}: conaryd is taking the repo's landing over")
            else:
                left.append(jid)
    if left:
        log(f"{repo}: letting conaryd have it once {', '.join(left)} stop")
        C.wake(repo, C.NOW + 5 * MIN)  # ends: the next pass stops them, or the record moves on
        return False
    release(repo, generation)
    return True


def release(repo, generation):
    """The release marker says the steward has nothing at work in the repo
    for this handover: written whole (a new file renamed over the old)."""
    if DRY:
        log(f"  would: release {repo}'s landing at generation {generation}")
        return
    path = os.path.join(CFG["stateDir"], MARKER)
    try:
        with open(path) as f:
            marker = json.load(f)
    except FileNotFoundError:
        marker = {}
    if marker.get(repo.lower()) == generation:
        return
    marker[repo.lower()] = generation
    pending = path + ".pending"
    with open(pending, "w") as f:
        json.dump(marker, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(pending, path)
    log(f"{repo}: nothing of the steward's at work there; released its landing to conaryd")


def lands(repo):
    """Before the repo's PRs: whether the steward lands them. Where it
    doesn't, it's said why; letting the repo go, it vacates first. Where
    the record can't be read, nobody lands, and the next pass looks again."""
    holding = C.landing(repo)
    if C.lands(holding):
        return True
    if holding is None:
        log(f"{repo}: its landing can't be read; none of its PRs is seen to")
        C.wake(repo, C.NOW + 5 * MIN)  # ends: the next pass reads it
    elif holding["owner"] == "steward" and holding["to"] == "conaryd":
        vacate(repo, holding["generation"])
    else:
        log(f"{repo}: {holding['owner']} lands its PRs" + (f", handing them to {holding['to']}" if holding["to"] else ""))
    return False
