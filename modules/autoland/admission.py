"""The steward's adapter to the shared Rust admission decision, and the
one-shot handoffs in it: a PR a trusted request handed to the steward while
its repo is paused (`settings land OWNER/REPO#N HEAD`, burrow's
docs/settings.md). A handoff admits that PR's own pass and its landing at
that head, nothing else: every Claude job, proof and review the repo would
start stays paused, so what it still needs keeps it waiting.

It also says who lands the repo's PRs (`landing`, burrow-settings'
landing.rs): the steward or conaryd. Every landing write (a merge, a train
write, a PR's own step and the jobs and proofs it starts) is gated by held(),
which no handoff overrides where the steward doesn't land the repo."""
import json
import subprocess


class JobPaused(Exception):
    """Intentional admission hold, never a failed launch or a job id."""


def decision(repo):
    """The repo's admission decision, or None when it can't be read (which
    pauses everything)."""
    import common as C

    settings = C.CFG.get("settings")
    if not settings:
        return None
    result = subprocess.run([settings["command"], settings["config"], "decision", repo],
                            capture_output=True, text=True)
    if result.returncode != 0:
        return None
    decision = json.loads(result.stdout)
    if decision["retryAt"] is not None:
        C.wake(repo, decision["retryAt"])  # ends: the scoped pause expires
    return decision


def landing(repo):
    """The repo's landing as the record has it ({"owner", "to",
    "generation"}), or None when the decision can't be read, which lets the
    steward land nothing."""
    d = decision(repo)
    return None if d is None else d["landing"]


def lands(holding):
    """Whether the steward may land in a repo with this landing: it holds it
    and no handover is under way. None (unreadable): no."""
    return holding is not None and holding["owner"] == "steward" and holding["to"] is None


def held(repo, pr=None):
    """paused(), for a landing write: true too wherever the steward doesn't
    land the repo, whatever the repo's admission or a handoff of the PR
    says, so it never lands beside conaryd."""
    d = decision(repo)
    if d is None or not lands(d["landing"]):
        return True
    return not d["allowed"] and not (pr is not None and handoff(d, pr))


def handoff(decision, pr):
    """The handoff in decision for this PR at its head, if there is one."""
    return next((h for h in (decision or {}).get("handed", ())
                 if h["pr"] == pr["number"] and h["head"] == pr["headRefOid"].lower()), None)


def paused(repo, pr=None):
    """Whether the repo's work is paused. With pr, for that PR's own pass and
    landing alone: a handoff of it at its head lets those through."""
    d = decision(repo)
    if d is None:
        return True
    return not d["allowed"] and not (pr is not None and handoff(d, pr))


def handed(repo, pr):
    """Whether the PR, at its head, is in a paused repo only by its handoff,
    in a repo the steward lands."""
    d = decision(repo)
    return d is not None and lands(d["landing"]) and not d["allowed"] and handoff(d, pr) is not None


def spend(repo, n, head):
    """The handoff of repo#n at head is used up: it landed, or its PR closed.
    Through the settings' own write door, which logs it; a newer handoff of
    the PR at another head stays. False when it couldn't be written (the
    next pass that finds its PR gone spends it then)."""
    import common as C

    settings = C.CFG["settings"]
    result = subprocess.run([settings["command"], settings["config"], "spent", f"{repo}#{n}", head],
                            capture_output=True, text=True)
    if result.returncode:
        C.log(f"{repo}#{n}: couldn't spend its handoff: {result.stderr.strip()[-300:]}")
    return result.returncode == 0


def spend_closed(repo, open_numbers, since):
    """Spend each of the repo's handoffs whose PR isn't open any more: it
    landed (by the train, or a merge whose spend didn't get written) or
    closed. Only those handed before since, when the open PRs were read
    after: one handed later may name a PR opened since."""
    for h in (decision(repo) or {}).get("handed", ()):
        if h["pr"] not in open_numbers and h["at"] < since:
            spend(repo, h["pr"], h["head"])
