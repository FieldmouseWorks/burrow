"""What a PR's new head stops: its Claude jobs still at work on the last
head, whose answers would be for a head it no longer has. Those are its
proofs, but for one building what the new head's proof wants
(proofs.superseded), and its judge. Its fix jobs carry on: they push to the
branch, whatever its head. A job stopped this way is forgotten as its
kind's, so a head the PR comes back to gets jobs of its own rather than
taking a stopped one for one that broke (step.py's lost). One proj won't
stop holds what it holds, and is tried again each pass, a strike each time
(step.py)."""

import common as C
import proofs as P
from common import log

KINDS = ("proved", "judged")  # the kinds of job a new head stops


def moved(repo, pr, st):
    """The PR's head moved on to pr's: its jobs to stop are marked (st's
    stopping), and forgotten."""
    old = P.superseded(repo, pr, st)
    if st.get("judged") and st.get("judgedJob") and P.running(st["judgedJob"]):
        old.append(st["judgedJob"])
    for kind in KINDS:
        if st.get(kind + "Job") in old:
            for k in (kind, kind + "Lost", kind + "Again"):
                st.pop(k, None)
    if old:
        st["stopping"] = list(dict.fromkeys(st.get("stopping", []) + old))


def stop(tag, st):
    """Stops the marked jobs still at work. False while proj refuses one,
    which stays marked; True once none is left."""
    left = []
    for jid in st.get("stopping", []):
        if not P.running(jid):
            continue
        if C.stop_job(jid):
            log(f"{tag}: stopped Claude job {jid}, at work on a head it no longer has")
        else:
            left.append(jid)
    if left:
        st["stopping"] = left
    else:
        st.pop("stopping", None)
    return not left
