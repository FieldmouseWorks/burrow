"""The reviewers' part of one PR's step (step.py): what each reviewer of the
PR's repo has said of its head, the answers back from their service posted as
reviews (reviewers.py, crates/burrow-review), the looks still owed it asked for,
and what that leaves the PR waiting on. step.py then decides on it, with its
threads, its CI and its gates."""

import os
from typing import NamedTuple

import common as C
import reviewers as V
from common import DRY, MIN, STATE, load, log, repo_cfg, save


class Looked(NamedTuple):
    """What the PR's reviewers come to on its head, for its step."""
    live: list  # the reviewers in service
    said: dict  # each one's verdict on this head: (verdict, when, rounds with findings)
    rounds: int  # the most rounds with findings any reviewer has
    clean: bool  # every one in service found nothing on this head
    capped: bool  # past the repo's round limit, and not clean
    unseen: list  # the paths the reviewers weren't shown
    late: list  # the heads whose findings this pass posted, short, that hold it back
    waiting: list  # what it still waits on from the reviewers, in words
    stuck: list  # the reviewers whose look keeps failing, in words
    out_of_service: bool  # what it waits on is a reviewer out of service


def look(repo, pr, key, st, lost_on, digest, line, age, after, strike, cleared):
    """The reviewers' part of the PR's step this pass. key, st and lost_on
    are its state file's name, its state and the findings its reviewers
    couldn't post; digest and line its listing; age how long its head has
    been seen; after, strike and cleared its step's own (step.py)."""
    rc = repo_cfg(repo)
    head = pr["headRefOid"]
    tag = f"{repo}#{pr['number']}"
    # The reviewers (reviewers.py): every one this repo has, but one out of
    # service (its usage spent, say) isn't waited for meanwhile. Each review
    # is queued for the reviewers' own service; a pass posts what comes back.
    names = V.on(repo)
    # What the reviewers weren't shown: their clean reviews don't cover it,
    # so a judge, who sees it all, looks before it lands.
    unseen = V.unseen(key, head, pr["baseRefName"])
    away = {r: V.out(r) for r in names}
    live = [r for r in names if not away[r]]
    for r in names:
        # Once a pass, whichever PR finds it out, while the others carry on
        # without it (with none left, the PR says so below): logging it back
        # in, say, is the owner's to do.
        if live and away[r] and not any(x[:2] == ("A reviewer is out of service", V.NAMES[r]) for x in digest):
            digest.append(("A reviewer is out of service", V.NAMES[r], away[r]))
    said = {r: V.verdict(pr, head, r) for r in names}  # verdict, when, rounds with findings
    found = []  # the reviewers whose findings on this head this pass posted
    late, unposted = [], []  # earlier heads whose findings this pass posted; reviews it couldn't post yet

    def went_out(r, back):
        o = load(V.OUT, {})
        o[r] = {"why": back["why"], "until": back["until"]}
        save(V.OUT, o)
        log(f"{tag}: {V.NAMES[r]} is out of service until {back['until']:.0f}: {back['why']}")

    def outs(r):
        """An earlier head's look that found the reviewer out of service:
        recorded before anything this pass decides on whether it's out."""
        for name in V.answered(key, r):
            back = load(f"{V.DONE}/{name}.json", None) or {}
            if name != V.item_name(key, r, head, pr["baseRefName"]) and back.get("result") == "out" and not DRY:
                went_out(r, back)
                V.drop(name)

    def consume(r):
        """What came back from the reviewer's service for this head, if
        anything: posted as its review, or counted as a failure."""
        rs = st["rv"].setdefault(r, {})
        name = V.item_name(key, r, head, pr["baseRefName"])
        back = load(f"{V.DONE}/{name}.json", None)
        if back and not DRY:
            if back["result"] == "review":
                try:
                    v = V.post(repo, pr, r, back["answer"], back.get("description"))
                    _, _, rr = said[r]
                    said[r] = (v if V.described(pr, back.get("description")) else "none", C.NOW, rr + (v == "findings"))
                    if v == "findings":
                        found.append(r)
                    rs.pop("fails", None)
                    rs.pop("failedAt", None)
                    cleared("post")
                    # A look at the PR's head covers what one at it or an
                    # earlier head on its base found.
                    lost_on.get(r, {}).pop(pr["baseRefName"], None)
                    if not lost_on.get(r, True):
                        lost_on.pop(r)
                except ValueError as e:  # an answer that's no review
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: {V.NAMES[r]}'s answer wasn't a review ({rs['fails']}): {e}")
                except C.Unavailable as e:  # GitHub didn't answer: no try spent, the answer's kept
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review, GitHub's unavailable: {e}")
                    unposted.append(f"{V.NAMES[r]}'s review of {head[:7]}")
                    strike("post", C.said(e))
                    return
                except RuntimeError as e:  # GitHub said no: the same answer goes again next pass
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review ({rs['fails']}): {e}")
                    # Three tries; one out of service then isn't asked again,
                    # so its answer stays, the pass waits and the owner hears.
                    if rs["fails"] < 3 or V.out(r):
                        x = f"{V.NAMES[r]}'s review of {head[:7]}"
                        unposted.append(x)
                        if rs["fails"] >= 3:
                            digest.append(("A review couldn't be posted", line, f"{x}, {rs['fails']} tries"))
                        strike("post", C.said(e))
                        return
                    if back["answer"].get("findings"):
                        # Dropped, it's asked again; one out of service by
                        # then isn't, so the head's held for it meanwhile.
                        lost_on.setdefault(r, {})[pr["baseRefName"]] = head
            elif back["result"] == "out":
                went_out(r, back)
            else:  # the agent, the network, its JSON: tried again next pass
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: {V.NAMES[r]}'s review failed ({rs['fails']}): {back.get('error', '')[:200]}")
            V.drop(name)
            rs.pop("queued", None)
            rs.pop("gone", None)
        elif rs.get("queued") and not DRY and not os.path.exists(os.path.join(STATE, V.QUEUE, name + ".json")):
            # Out of the queue with nothing back. However long an agent
            # thinks, its review stays queued until it's answered, so this
            # is a lost one; seen on two passes, since the service may have
            # answered between this pass's two looks.
            if rs.get("gone"):
                rs.pop("queued")
                rs.pop("gone")
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: the queued {V.NAMES[r]} review left the queue without an answer")
            else:
                rs["gone"] = True
                after(C.NOW, 1)  # ends: the second look, once

    def overtaken(r, over):
        """What came back from the reviewer's looks at the PR's earlier heads
        after it moved on (V.sweep keeps them). Such a look can't know what
        the pushes since fixed, so while the reviewer is still to look at
        this head, that look decides and this one is dropped: posted, it
        would raise again what was fixed meanwhile, and cost a round. One
        the reviewer won't follow with a look at this head (it's out of
        service, or the PR is past the round limit: `over`) is its last word
        on the PR, so it's posted on the commit it read: findings there
        count as a round, and hold this pass back from landing, since the
        threads it read are from before them. "Nothing found" there says
        nothing about this head."""
        rs = st["rv"].setdefault(r, {})
        this = V.item_name(key, r, head, pr["baseRefName"])
        mine = {name: load(f"{V.DONE}/{name}.json", None) or {} for name in V.answered(key, r)}
        tries = {n: k for n, k in rs.get("late", {}).items() if n in mine}
        # Whether it's out decides what's dropped, so every outage listed is
        # recorded first, wherever it sorts: an earlier head's, in since
        # outs() looked, and this head's, in since consume() read it.
        for name, back in mine.items():
            if back.get("result") == "out" and not DRY:
                if name == this:
                    consume(r)
                else:
                    went_out(r, back)
                    V.drop(name)
        for name, back in mine.items():
            if name == this or DRY or back.get("result") == "out":
                continue
            if back.get("result") == "review" and back.get("base") == pr["baseRefName"] and not (over or V.out(r)):
                log(f"{tag}: dropped {V.NAMES[r]}'s review of {str(back.get('head'))[:7]}, back after the PR moved on: "
                    f"its look at {head[:7]} decides")
            elif back.get("result") == "review" and back.get("base") == pr["baseRefName"] and isinstance(back.get("head"), str):
                try:
                    if V.post(repo, {**pr, "headRefOid": back["head"]}, r, back["answer"], back.get("description")) == "findings":
                        v, at, rr = said[r]
                        said[r] = (v, at, rr + 1)
                        late.append(back["head"][:7])
                    cleared("post")
                except ValueError as e:  # an answer that's no review
                    log(f"{tag}: {V.NAMES[r]}'s review of {back['head'][:7]} wasn't a review: {e}")
                except C.Unavailable as e:  # GitHub didn't answer: no try spent, the answer's kept
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review of {back['head'][:7]}, GitHub's unavailable: {e}")
                    unposted.append(f"{V.NAMES[r]}'s review of {back['head'][:7]}")
                    strike("post", C.said(e))
                    continue
                except RuntimeError as e:  # GitHub said no: the same answer goes again next pass
                    tries[name] = tries.get(name, 0) + 1
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review of {back['head'][:7]} ({tries[name]}): {e}")
                    # Three tries, and then only until the reviewer finds this
                    # head clean, which covers what it found there; one out of
                    # service may never, so the owner hears.
                    if tries[name] < 3 or said[r][0] != "clean":
                        x = f"{V.NAMES[r]}'s review of {back['head'][:7]}"
                        unposted.append(x)
                        if tries[name] >= 3:
                            digest.append(("A review couldn't be posted", line, f"{x}, {tries[name]} tries"))
                        strike("post", C.said(e))
                        continue
            elif (back.get("result") == "review" and isinstance(back.get("base"), str) and isinstance(back.get("head"), str)
                  and isinstance(back.get("answer"), dict) and back["answer"].get("findings")):
                # Findings on another base, which the PR left while the look
                # was under way: not posted here, but owed if it goes back.
                lost_on.setdefault(r, {})[back["base"]] = back["head"]
            tries.pop(name, None)
            V.drop(name)
        if tries:
            rs["late"] = tries
        else:
            rs.pop("late", None)

    for r in names:
        outs(r)

    def reading(rs):
        """Those of `rs` with a look at the PR, at any head, still under way:
        out of service, their verdict on this head doesn't cover it, so the
        pass waits for what it finds. One not begun never will be. Asked
        before this pass reads what came back, so one that ends meanwhile
        is read."""
        return [r for r in rs if not DRY and said[r][0] != "clean" and V.under_way(key, r, pr["baseRefName"])]

    def limit():
        """The most rounds with findings any reviewer has, in service or not
        (one going out doesn't take a PR back under the limit), whether
        every one in service found nothing on this head, and whether that
        leaves it past the repo's round limit (capped). Past it, the
        reviewers aren't asked again: every look so far has found more, so
        more looks only drip-feed more rounds. A Claude job settles what's
        left on its merits, fixing what's real and answering the rest, and a
        judge decides whether it lands. Only what the judge can't call (a
        one-way door) waits for the owner."""
        rounds = max((said[r][2] for r in names), default=0)
        on = [r for r in live if not V.out(r)]
        clean = bool(on) and all(said[r][0] == "clean" for r in on)
        return rounds, clean, rounds >= rc["rounds"] and not clean

    was_out = [r for r in names if V.out(r)]
    under_way = reading(was_out)
    for r in names:
        # One out of service too, or put out by one of these answers: its
        # looks came back before it went out, and posting them asks nothing
        # of it.
        consume(r)
    over = limit()[2]  # with this head's answers in: no look at it follows
    for r in names:
        overtaken(r, over)
    # Put out by an answer this pass read: its others.
    under_way += reading([r for r in names if V.out(r) and r not in was_out])
    live = [r for r in live if not V.out(r)]
    for r in names:
        if V.out(r):
            # Back in service then: its look at this head is asked for.
            C.wake(repo, load(V.OUT, {})[r]["until"] + 1)  # ends: once; listed while it's out
    rounds, clean, capped = limit()

    def owed(r):
        """Past the limit, what the reviewer still owes the PR, which the
        judge mustn't land past: a look at any of its heads on this base
        still in the queue (begun or not, it's waited for), an answer back
        but not yet posted, or findings that couldn't be posted, at this
        head or an earlier one (it's asked again on this head: that's the
        same round, not another)."""
        base = pr["baseRefName"]
        return bool(V.in_queue(key, r, base) or V.answered(key, r) or base in lost_on.get(r, {}))
    # Findings on this head this pass posted are threads it never read: one
    # out now isn't waited for, and past the limit no look follows to cover
    # them, so they hold the pass back.
    late += [head[:7] for r in found if r not in live or capped]

    # 1. The reviewers.
    quiet = age >= rc["settleDelay"] * MIN
    looking = [r for r in live if owed(r)] if capped else [r for r in live if said[r][0] == "none"]
    if not quiet and looking:
        after(st["seen"], rc["settleDelay"])  # ends: the reviewers are asked then, once a head
    waiting = [f"Posting {x}" for x in unposted]
    # It waits on a reviewer out of service: nothing moves it until that one
    # is back. (One still reading it, though out, is a look under way.)
    out_of_service = False
    for r in names:
        if r in live:
            continue
        if lost_on.get(r, {}).get(pr["baseRefName"]) == head:
            waiting.append(f"{V.NAMES[r]}'s findings on {head[:7]} couldn't be posted, and it's out of service")
            out_of_service = True
        elif capped and owed(r) and r not in under_way:
            waiting.append(f"{V.NAMES[r]} still owes it a review, and it's out of service")
            out_of_service = True
    waiting += [f"{V.NAMES[r]} is still reviewing it, though out of service" for r in under_way]
    if not live and not capped:
        why = "; ".join(f"{V.NAMES[r]}: {away[r] or V.out(r)}" for r in names) or "none is on"
        waiting.append(f"No reviewer can review it now ({why})")
        digest.append(("No reviewer can review it", line, why))
        out_of_service = out_of_service or bool(names)
    for r in looking:
        if quiet and (not capped or pr["baseRefName"] in lost_on.get(r, {})):
            V.ask(repo, pr, key, r, st["rv"].setdefault(r, {}), after)
        waiting.append(f"{V.NAMES[r]} is reviewing this commit" if st["rv"].get(r, {}).get("queued")
                       else f"Waiting for {V.NAMES[r]}'s review" + ("" if capped else " of this commit"))
    # A look that can't happen, a first one or a second: the owner hears either way.
    stuck = []
    for r in live:
        if st["rv"].get(r, {}).get("fails", 0) >= 3:
            x = f"failed {st['rv'][r]['fails']} times on {head[:7]}"
            stuck.append(f"{V.NAMES[r]} {x}")
            digest.append((f"{V.NAMES[r]} couldn't review it", line, x + " (journalctl -u autoland-review)"))

    return Looked(live, said, rounds, clean, capped, unseen, late, waiting, stuck, out_of_service)
