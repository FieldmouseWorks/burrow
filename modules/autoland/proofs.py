"""A repo's proofSlots: how many of its open PRs may have a proof job queued
or running at once, for proofs that take the whole machine (the rest would
only wait out their timeout). The others wait their turn, oldest PR first,
and the end of a job's turn kicks the repo's pass, which starts the next.

What a slot is held by is a build: a running proof job with store paths
still to build that the jobs started before it aren't building (its
outputs, from the repo's proofBuilds). Nix builds a path once and the
others wait for it, so a job whose paths are all built, or all being built
by jobs that started first, holds nothing; a job whose paths nobody worked
out holds a slot of its own until it ends. So a proof with nothing new to
build skips the line: all its paths are built, or being built by proofs
already running.

A job is still a job whatever head it was started on: a PR pushed again,
or marked a draft, while its job runs still has what it holds. A turn in
the line is only this pass's: a PR is ahead while it's waiting in the same
pass (they go in number order), so one held, drafted or done since holds
nobody up."""

import os
import re
import subprocess
import time
import urllib.parse

import common as C
import reviewers as V
from common import CFG, STATE, load, log

OPEN = {}  # the open PRs of each repo this pass reads (begin)
WAITING = {}  # the PRs of each repo this pass found waiting their turn
FREE = "nothing new to build"  # ahead's answer for a proof that skips the line
EVAL_TIMEOUT = 300  # seconds, its inputs fetched included


def begin(repo, numbers):
    """A pass over the repo's open PRs starts: only theirs count."""
    OPEN[repo] = set(numbers)
    WAITING[repo] = set()


def jobs(st):
    """A PR's proof jobs that may still hold a slot: every one started since
    the last look (proofsLive), and its latest (from before there was one)."""
    latest = [st["provedJob"]] if st.get("provedJob") else []
    return list(dict.fromkeys(st.get("proofsLive", []) + latest))


def running(jid):
    return C.job_state(jid) in (None, "queued", "running")


def live(st):
    """The PR's proof jobs still queued or running, or that proj can't say
    about: kept in its state, the ended ones dropped, and what they build
    with them."""
    st["proofsLive"] = [j for j in jobs(st) if running(j)]
    if "proofPaths" in st:  # {job: {"paths": [...], "at": when it started}}
        st["proofPaths"] = {j: p for j, p in st["proofPaths"].items() if j in st["proofsLive"]}
    return st["proofsLive"]


def nix(*args, timeout=60):
    p = subprocess.run([f"{CFG['nix']}/{args[0]}", *args[1:]], capture_output=True, text=True, timeout=timeout)
    if p.returncode != 0:
        raise RuntimeError(f"{args[0]}: {p.stderr.strip()[-300:]}")
    return p.stdout


def outputs(repo, pr, checks, st):
    """The store paths PR's proof of checks builds on its head: each check's
    flake attribute (the repo's proofBuilds) at that exact commit, from the
    steward's mirror, as the proof job's checkout of it has it (its rev
    included), with import-from-derivation off. Worked out once a head,
    and None when it can't be: a check with no attribute, an eval that
    fails or takes too long."""
    attrs = C.repo_cfg(repo)["proofBuilds"]
    if C.DRY or not checks or not all(c in attrs for c in checks):
        return None
    n, head = pr["number"], pr["headRefOid"]
    key = [head, {c: attrs[c] for c in sorted(checks)}]
    got = st.get("proofOutputs")
    if got and got["for"] == key:
        return got["paths"]
    paths = None
    try:
        mirror = V.fetch(repo, f"+refs/pull/{n}/head:refs/pr/{n}")
        url = f"git+file://{urllib.parse.quote(mirror)}?ref=refs/pr/{n}&rev={head}"
        drvs = [nix("nix", "--extra-experimental-features", "nix-command flakes", "eval", "--raw",
                    "--option", "allow-import-from-derivation", "false", "--option", "accept-flake-config", "false",
                    "--no-update-lock-file", f"{url}#{attrs[c]}.drvPath", timeout=EVAL_TIMEOUT) for c in sorted(checks)]
        paths = sorted(set(nix("nix-store", "--query", "--outputs", *drvs).split())) or None
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        log(f"{repo}#{n}: can't tell what its proof builds at {head[:12]}: {e}")
    st["proofOutputs"] = {"for": key, "paths": paths}
    return paths


def unbuilt(paths):
    """Those of the paths not valid in this machine's store (no substituter
    asked): all of them, when the store can't say."""
    if not paths:
        return set()
    try:
        return set(nix("nix-store", "--check-validity", "--print-invalid", *sorted(paths)).split())
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        log(f"can't tell which store paths are built: {e}")
        return set(paths)


def ahead(repo, n, st, slots, outs=None):
    """How many proofs are ahead of PR n's: the builds every open PR's live
    jobs, its own included, are running, and the lower-numbered PRs waiting
    their turn this pass. None when its job may start now; FREE when it may
    whatever the line, since all it builds (outs, None when unknown) is
    built or being built already."""
    def held(state, js):
        return [(rec.get("at", 0), j, rec.get("paths")) for j in js
                for rec in [state.get("proofPaths", {}).get(j) or {}]]

    jobs_now = held(st, live(st))
    mine = re.escape(f"pr-{repo.replace('/', '_')}-") + r"(\d+)\.json"
    for f in os.listdir(STATE):
        m = re.fullmatch(mine, f)
        if m and int(m[1]) != n and (repo not in OPEN or int(m[1]) in OPEN[repo]):
            other = load(f, {})
            jobs_now += held(other, [j for j in jobs(other) if running(j)])
    todo = unbuilt({p for _, _, ps in jobs_now if ps for p in ps} | set(outs or ()))
    # In the order they started: a job is a build when it has paths still
    # to build that those before it aren't building, or when they're
    # unknown.
    builds, building = 0, set()
    for _, _, ps in sorted(jobs_now, key=lambda x: x[:2]):
        rest = todo.intersection(ps) if ps is not None else None
        builds += rest is None or not rest <= building
        building |= rest or set()
    waiting = sum(other < n for other in WAITING.get(repo, ()))
    if builds + waiting < slots:
        return None
    if outs is not None and todo.intersection(outs) <= building:
        return FREE
    return builds + waiting


def wait(repo, n):
    """PR n waits its turn this pass: the ones after it wait behind it."""
    WAITING.setdefault(repo, set()).add(n)


def started(st, outs=None):
    """The PR's latest proof job, just started (or not): it holds what it
    builds until it ends, whatever job starts after it. outs: what a job
    started just now builds, when that's known."""
    st["proofsLive"] = jobs(st)
    if outs is not None:
        # The clock, not the pass's: jobs one pass starts come in order.
        st.setdefault("proofPaths", {})[st["provedJob"]] = {"paths": outs, "at": time.time()}
