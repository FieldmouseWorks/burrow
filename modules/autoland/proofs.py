"""A repo's proofSlots: how many of its open PRs may have a proof job queued
or running at once, for proofs that take the whole machine (the rest would
only wait out their timeout). The others wait their turn, the PRs labelled
`first` (labels.first) ahead of the rest and the oldest first within each,
and the end of a job's turn kicks the repo's pass, which starts the next.

What a slot is held by is a build: a running proof job with store paths
still to build that the jobs started before it aren't building (its
outputs, from the repo's proofBuilds). Nix builds a path once and the
others wait for it, so a job whose paths are all built, or all being built
by jobs that started first, holds nothing; a job whose paths nobody worked
out holds a slot of its own until it ends. So a proof with nothing new to
build skips the line: all its paths are built, or being built by proofs
already running.

One whose paths are all built takes no job at all. A path is valid in the
store only once its build has passed, so its checks' verdicts are taken
from there (built, record), saying so and naming each derivation: that
exact derivation passed on this machine, once, and isn't run again.

A check with a proofBuilds attribute is that build and nothing more, so
the steward builds it itself (build): no Claude job and no proj slot, but a
unit of the machine user's own that runs `nix-store --realise` on each of
its derivations in a heavy slot (run, realise). Each check's verdict is
how its derivation's realise ended, kept the moment the build ends
(verdicts), and a failure's log tail goes on the PR. A build holds a slot
as a job does, and is asked after and stopped like one (common.job_state,
stop_job). Checks without an attribute are a Claude job's, after the
builds: a failed build needs no job.

A check with a proofRuns command is that command's exit and nothing more,
so the steward runs it itself the same way (start_run): the same unit, no
Claude job, but each command in a fresh worktree of the repo's checkout at
the PR's head (commands), its exit the verdict. What the command writes to
its PROOF_SUMMARY file goes on the PR; its log stays on the machine, so
nothing it prints reaches GitHub but what it chose to write there (note).
Runs go after the builds and before the Claude jobs: a failed run needs no
job either.

A PR behind its base whose builds aren't in the store may have them there
once it's brought up to date: most PRs change nothing a proof builds, so
merged into the base's tip they build what the base does, which is built
already. Then the steward brings it up to date instead of building it
(merged), and the new head's verdicts come from the store.

A job holds what it holds until it ends, whatever its PR does meanwhile
(marked a draft, say), but for a new head: a verdict on a head the PR no
longer has proves nothing, so the steward stops it (superseded), unless
it's building what the new head's proof wants. A turn in the line is only
this pass's: a PR is ahead while it's waiting in the same pass (they go in
their line's order, place), so one held, drafted or done since holds
nobody up."""

import os
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.parse

import common as C
import reviewers as V
from common import CFG, STATE, load, log, save

OPEN = {}  # the open PRs of each repo this pass reads (begin)
FIRST = {}  # those of them labelled to go first
WAITING = {}  # the PRs of each repo this pass found waiting their turn
FREE = "nothing new to build"  # ahead's answer for a proof that skips the line
EVAL_TIMEOUT = 300  # seconds, its inputs fetched included
EVAL_RETRY = 15  # minutes before an eval that failed on a head is tried again
LOG_TAIL = 60  # lines of a failed build's log that go on its PR
# The most a failed build's comment may take, in UTF-8 bytes: under GitHub's
# 65,536 characters, and under the 128 KiB Linux takes for the one argument
# gh is given it in.
COMMENT_BYTES = 60000
KEEP = 7  # days an ended build's folder, its outputs' roots in it, is kept


def begin(repo, numbers, first=()):
    """A pass over the repo's open PRs starts: only theirs count, and those
    in first go ahead of the rest."""
    OPEN[repo] = set(numbers)
    FIRST[repo] = set(first)
    WAITING[repo] = set()


def place(repo, n):
    """Where PR n stands in its repo's line this pass: one labelled first
    ahead of every one that isn't, and the oldest first within each. The
    pass takes the PRs in this order, so a slot goes to the first in line."""
    return n not in FIRST.get(repo, ()), n


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


def derivations(repo, mirror, ref, rev, checks):
    """Each of checks' derivations, by check, and all their outputs: its
    flake attribute (the repo's proofBuilds) at that exact commit of the
    steward's mirror, as a proof's checkout of it has it (its rev
    included), with import-from-derivation off. Raises when it can't be
    worked out."""
    attrs = C.repo_cfg(repo)["proofBuilds"]
    url = f"git+file://{urllib.parse.quote(mirror)}?ref={ref}&rev={rev}"
    drvs = {c: nix("nix", "--extra-experimental-features", "nix-command flakes", "eval", "--raw",
                   "--option", "allow-import-from-derivation", "false", "--option", "accept-flake-config", "false",
                   "--no-update-lock-file", f"{url}#{attrs[c]}.drvPath", timeout=EVAL_TIMEOUT) for c in sorted(checks)}
    return drvs, sorted(set(nix("nix-store", "--query", "--outputs", *drvs.values()).split()))


def outputs(repo, pr, checks, st):
    """The store paths PR's proof of checks builds on its head (derivations).
    Worked out once a head, and None when it can't be: a check with no
    attribute, an eval that fails or takes too long (tried again EVAL_RETRY
    minutes on)."""
    attrs = C.repo_cfg(repo)["proofBuilds"]
    if C.DRY or not checks or not all(c in attrs for c in checks):
        return None
    n, head = pr["number"], pr["headRefOid"]
    key = [head, {c: attrs[c] for c in sorted(checks)}]
    got = st.get("proofOutputs")
    if got and got["for"] == key and (got["paths"] or C.NOW - got.get("at", 0) < EVAL_RETRY * C.MIN):
        # Derivations collected since are worked out again, which puts them
        # back in the store.
        if not got["paths"] or not gone(sorted(set((got.get("drvs") or {}).values()))):
            return got["paths"]
    drvs = paths = None
    try:
        mirror = V.fetch(repo, f"+refs/pull/{n}/head:refs/pr/{n}")
        drvs, paths = derivations(repo, mirror, f"refs/pr/{n}", head, checks)
        paths = paths or None
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        log(f"{repo}#{n}: can't tell what its proof builds at {head[:12]}: {e}")
    st["proofOutputs"] = {"for": key, "paths": paths, "drvs": drvs if paths else None, "at": C.NOW}
    return paths


def merged(repo, pr, checks, st, tip):
    """The store paths PR's proof of checks would build once it's brought
    up to date with its base, whose tip GitHub says is tip: its head merged
    into tip, as GitHub's update-branch merges it (a clean merge only).
    Worked out once a head on a tip, and None when it's up to date already,
    the merge conflicts, or what it builds can't be worked out (tried again
    EVAL_RETRY minutes on)."""
    attrs = C.repo_cfg(repo)["proofBuilds"]
    if C.DRY or not tip or not checks or not all(c in attrs for c in checks):
        return None
    n, head, base = pr["number"], pr["headRefOid"], pr["baseRefName"]
    key = [head, tip, {c: attrs[c] for c in sorted(checks)}]
    got = st.get("mergedOutputs")
    if got and got["for"] == key and (not got.get("failed") or C.NOW - got.get("at", 0) < EVAL_RETRY * C.MIN):
        return got["paths"]
    paths, failed = None, True
    try:
        mirror = V.fetch(repo, f"+refs/pull/{n}/head:refs/pr/{n}", f"+refs/heads/{base}:refs/base/{base}")
        up = subprocess.run(["git", "-C", mirror, "merge-base", "--is-ancestor", tip, head], capture_output=True, text=True)
        # 0: the head has the tip already; 1: it doesn't; else git can't say
        # (a tip that moved on again before the fetch, say).
        if up.returncode not in (0, 1):
            raise RuntimeError(f"git merge-base: {up.stderr.strip()[-300:]}")
        tree = None
        if up.returncode == 1:
            tree = subprocess.run(["git", "-C", mirror, "merge-tree", "--write-tree", "--no-messages", tip, head],
                                  capture_output=True, text=True)
            # 1 is a merge that conflicts: nothing to bring it up to date to.
            if tree.returncode not in (0, 1):
                raise RuntimeError(f"git merge-tree: {tree.stderr.strip()[-300:]}")
        if tree and tree.returncode == 0:
            who = ["-c", "user.name=autoland", "-c", "user.email=autoland@localhost", "-c", "commit.gpgsign=false"]
            rev = V.git("-C", mirror, *who, "commit-tree", tree.stdout.split()[0], "-p", head, "-p", tip,
                        "-m", f"#{n} brought up to date with {base}").decode().strip()
            V.git("-C", mirror, "update-ref", f"refs/merged/{n}", rev)
            paths = derivations(repo, mirror, f"refs/merged/{n}", rev, checks)[1] or None
        failed = False
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        log(f"{repo}#{n}: can't tell what its proof builds brought up to date with {base}: {e}")
    st["mergedOutputs"] = {"for": key, "paths": paths, "failed": failed, "at": C.NOW}
    return paths


def built(repo, pr, checks, st):
    """Each of checks' derivations on PR's head, by check, when every output
    of them all is valid in this machine's store: each passed here. None
    when one isn't, or what they build can't be worked out."""
    outs = outputs(repo, pr, checks, st)
    if not outs or unbuilt(outs):
        return None
    return st["proofOutputs"].get("drvs")


def stored(checks, drvs):
    """Verdicts for record from built's derivations: each a pass that says
    it was taken from the store and names its derivation, so it doesn't
    read as a run."""
    return {c: ("success", f"Taken from the store, not run again: {drvs[c]} passed here before") for c in checks}


def record(repo, pr, verdicts):
    """Verdicts ({check: (state, description)}) on PR's head, as its
    statuses. GitHub's word on each check, read fresh, says which to post:
    only one still waiting, so a pass that read it just before the last
    one's status posts nothing again, and one added, remapped or set back
    to pending since is posted, and a red one is left red. A context is the
    same one however it's spelled: GitHub ignores its case. False if GitHub
    didn't answer or take one: what's left is posted next time."""
    head = pr["headRefOid"]
    now = C.gh("api", f"repos/{repo}/commits/{head}/status?per_page=100", check=False)
    if not isinstance(now, dict):
        return False
    settled = {s["context"].lower() for s in now.get("statuses", []) if s["state"] != "pending"}
    # Passes first: a failure GitHub took turns the PR red, and the passes
    # left behind it would wait on a pass that keeps posting (step.py).
    for c, (state, desc) in sorted(verdicts.items(), key=lambda v: v[1][0] != "success"):
        if c.lower() in settled:
            continue
        if C.write("api", f"repos/{repo}/statuses/{head}", "-f", f"state={state}", "-f", f"context={c}",
                   "-f", f"description={desc[:140]}", "-f", f"target_url={pr['url']}", check=False) is None:
            return False
    return True


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
    jobs, its own included, are running, and the PRs ahead of it in line
    waiting their turn this pass. None when its job may start now; FREE
    when it may whatever the line, since all it builds (outs, None when
    unknown) is built or being built already."""
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
    waiting = sum(place(repo, other) < place(repo, n) for other in WAITING.get(repo, ()))
    if builds + waiting < slots:
        return None
    if outs is not None and todo.intersection(outs) <= building:
        return FREE
    return builds + waiting


def superseded(repo, pr, st):
    """The PR's head moved on to pr's: of its proof jobs still at work, all
    on a head it no longer has, the ones to stop. That's every one but a job
    building something the new head's proof wants (its check, unchanged),
    which that proof would only wait for (ahead): it's left to finish. One
    whose builds nobody worked out is stopped, and so is every one when the
    new head's can't be worked out."""
    js = live(st)
    recs = st.get("proofPaths", {})
    wanted = set()
    if any((recs.get(j) or {}).get("paths") for j in js) and (last := st.get("proofOutputs")):
        wanted = unbuilt(outputs(repo, pr, sorted(last["for"][1]), st))
    keep = [j for j in js if wanted.intersection((recs.get(j) or {}).get("paths") or ())]
    for j in keep:
        log(f"{repo}#{pr['number']}: proof job {j}, on its last head, builds what this head's proof wants: left to finish")
    return [j for j in js if j not in keep]


def build(repo, pr, checks, st):
    """Starts building checks' derivations on PR's head (outputs worked
    them out: they're in st) in a unit of the machine user's own, which
    runs run(): no Claude job. Its id (common.BUILD and more), or None when
    systemd wouldn't start it."""
    n, head = pr["number"], pr["headRefOid"]
    drvs = {c: st["proofOutputs"]["drvs"][c] for c in checks}
    jid = unit_id(repo, n, head)
    if C.DRY:
        log(f"  would: build {', '.join(sorted(set(drvs.values())))}")
        return jid
    try:
        # Each check's outputs, while its derivation is surely in the store
        # (just worked out): the build's verdicts are read from them alone.
        outs = {c: nix("nix-store", "--query", "--outputs", d).split() for c, d in drvs.items()}
    except (RuntimeError, OSError, subprocess.SubprocessError) as e:
        log(f"{repo}#{n}: can't tell what the build of {', '.join(checks)} makes: {e}")
        return None
    return start(jid, {"repo": repo, "number": n, "head": head, "drvs": drvs, "outs": outs},
                 f"the build of {', '.join(checks)}")


def start_run(repo, pr, checks, st):
    """Starts running checks' commands (the repo's proofRuns) on PR's head
    in the same unit a build has (run): no Claude job. Each runs in a
    worktree of the repo's checkout here (common.project_dir), the one its
    Claude jobs work in. Its id, or None when there's no checkout or
    systemd wouldn't start it."""
    n, head = pr["number"], pr["headRefOid"]
    runs = {c: C.repo_cfg(repo)["proofRuns"][c] for c in checks}
    jid = unit_id(repo, n, head)
    if C.DRY:
        log(f"  would: run {', '.join(sorted(runs))}")
        return jid
    checkout = C.project_dir(repo)
    if checkout is None:
        log(f"{repo}#{n}: no checkout of it here to run {', '.join(checks)} in")
        return None
    return start(jid, {"repo": repo, "number": n, "head": head, "base": pr["baseRefName"], "checkout": checkout,
                       "runs": runs}, f"the run of {', '.join(checks)}")


def unit_id(repo, n, head):
    return f"{C.BUILD}{re.sub('[^a-z0-9]+', '-', repo.lower())}-{n}-{head[:12]}-{time.time_ns()}"


def start(jid, build_json, what):
    """The proof's folder, with what it is (build.json), then its unit,
    which runs run(). Its id, or None when systemd wouldn't start it."""
    prune()
    os.makedirs(os.path.join(STATE, "builds", jid))
    save(f"builds/{jid}/build.json", build_json)
    p = C.user_systemctl_run(C.build_unit(jid), own("build", jid))
    if p.returncode:
        log(f"{build_json['repo']}#{build_json['number']}: couldn't start {what}: {p.stderr.strip()[-300:]}")
        return None
    return jid


def own(*args):
    """A command line of this steward's own (`autoland-steward ARGS`), which
    reads the config its caller was given."""
    return [sys.executable, "-E", "-s", os.path.join(os.path.dirname(os.path.abspath(__file__)), "steward.py"), *args]


def prune():
    """Ended builds' folders, and the roots of their outputs in them, KEEP
    days after they ended: their verdicts are long posted by then."""
    root = os.path.join(STATE, "builds")
    for jid in os.listdir(root) if os.path.isdir(root) else ():
        end = load(f"builds/{jid}/end.json", None)
        if end and time.time() - end["at"] > KEEP * 86400:
            shutil.rmtree(os.path.join(root, jid), ignore_errors=True)


def run(jid):
    """The proof itself, as its unit runs it (`autoland-steward build ID`):
    a build's or a run's (commands), how it ended in end.json, then a kick
    of its repo's pass, which posts its verdicts."""
    b = load(f"builds/{jid}/build.json", None)
    end = commands(jid, b) if "runs" in b else realised(jid, b)
    save(f"builds/{jid}/end.json", {**end, "at": time.time()})
    C.kick(b["repo"])


def realised(jid, b):
    """A build's end: realise in a heavy slot, its log beside it. Done once
    nix built all it could (each realise's exit 0, or a build failure's
    100-107): each check's verdict is then kept in end.json (judged), so
    nothing collected later can change it. Failed when nix couldn't build
    at all, or what it built isn't there (the machine's failure, not the
    PR's, so it's built again); timed out past proofTimeout."""
    # Derivations collected since they were worked out leave nothing to
    # build, and no verdict in that: the pass that sees it works them out
    # again.
    if gone(sorted(set(b["drvs"].values()))):
        return {"state": "failed", "exit": None}
    cmd = ([CFG["heavy"], "--"] if CFG.get("heavy") else []) + own("realise", jid)
    with open(os.path.join(STATE, "builds", jid, "log"), "wb") as out:
        p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT)
        try:
            rc = p.wait(timeout=CFG["proofTimeout"] * C.MIN)
        except subprocess.TimeoutExpired:
            p.terminate()  # heavy stops all of it, then frees its slot
            p.wait()
            rc = None
    end = {"state": state_of(rc), "exit": rc}
    return {**end, **judged(jid, b)} if end["state"] == "done" else end


# A run's command: 0 passes, 1 to this fails. Anything else, a signal's
# (killed, or a shell's 128 and up), a command not found or not runnable
# (127, 126), is the machine's failure, not a verdict: it's run again.
RUN_FAILED = range(1, 126)


def commands(jid, b):
    """A run's end: each check's command in turn (the repo's proofRuns, by
    check name), in bash, in a fresh detached worktree of the repo's
    checkout at the PR's head under runsDir, removed once it ends. Its
    environment is what the machine user's own units start with (proj's
    jobs too; user_path), with PROOF_REPO, PROOF_NUMBER, PROOF_HEAD,
    PROOF_BASE (origin/BASE fetched just before) and PROOF_CHECK, and
    PROOF_SUMMARY, a file whose words go on the PR (note). Everything it
    prints goes to the run's log, on this machine only. It takes no heavy
    slot: a command takes the locks it needs itself. Done once each ran,
    each check's verdict its exit (RUN_FAILED); failed when one couldn't
    (the head can't be fetched, the user's PATH can't be read, a command
    killed); timed out past proofTimeout, all of them together. A stop
    (SIGTERM) ends the command and removes its worktree too."""
    here = os.path.join(STATE, "builds", jid)
    git = ["git", "-C", b["checkout"]]
    deadline = time.monotonic() + CFG["proofTimeout"] * C.MIN
    stopped = []

    def stop(*_):
        stopped.append(True)
        raise Stopped()

    signal.signal(signal.SIGTERM, stop)
    verdicts, exits = {}, {}
    with open(os.path.join(here, "log"), "ab") as out:
        def note_line(text):
            out.write(f"autoland: {text}\n".encode())
            out.flush()

        try:
            sweep(b["checkout"])
            path = user_path()
            if path is None:
                note_line("can't read the PATH the machine user's units start with (systemctl --user show-environment)")
                return {"state": "failed", "exit": None}
            # The head (refs/pull/N/head names it on GitHub) and the base,
            # which updates origin/BASE as any fetch of it does.
            fetch = subprocess.run([*git, "fetch", "-q", "origin", f"refs/pull/{b['number']}/head", b["base"]],
                                   stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT)
            have = subprocess.run([*git, "cat-file", "-e", f"{b['head']}^{{commit}}"], capture_output=True)
            if fetch.returncode or have.returncode:
                note_line(f"can't fetch {b['head']} into {b['checkout']}")
                return {"state": "failed", "exit": None}
            for i, (c, cmd) in enumerate(sorted(b["runs"].items())):
                note_line(f"{c}: {cmd}")
                rc = one(jid, b, c, cmd, i, path, out, deadline)
                note_line(f"{c}: {'past proofTimeout' if rc is None else f'exit {rc}'}")
                if rc is None:
                    return {"state": "timed out", "exit": None}
                if rc != 0 and rc not in RUN_FAILED:
                    return {"state": "failed", "exit": rc}
                verdicts[c] = ["success", "Ran here and passed"] if rc == 0 else ["failure", f"Ran here and failed: exit {rc}"]
                exits[c] = rc
        except Stopped:
            note_line("stopped")
            return {"state": "failed", "exit": None}
    return {"state": "done", "exit": 0, "verdicts": verdicts, "exits": exits}


class Stopped(Exception):
    """The run's unit was stopped (SIGTERM)."""


def one(jid, b, check, cmd, i, path, out, deadline):
    """One check's command (commands): its exit, None past the deadline.
    Its worktree goes once it ends, however it ends."""
    tree = os.path.join(CFG["runsDir"], f"{jid}-{i}")
    git = ["git", "-C", b["checkout"]]
    add = subprocess.run([*git, "worktree", "add", "-q", "--detach", tree, b["head"]],
                         stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT)
    if add.returncode:
        out.write(f"autoland: {check}: can't make its worktree at {tree}\n".encode())
        shutil.rmtree(tree, ignore_errors=True)
        return -1  # the machine's failure (RUN_FAILED)
    env = {k: v for k, v in os.environ.items() if k != "AUTOLAND_CONFIG"}
    env.update(PATH=path, PROOF_REPO=b["repo"], PROOF_NUMBER=str(b["number"]), PROOF_HEAD=b["head"],
               PROOF_BASE=b["base"], PROOF_CHECK=check,
               PROOF_SUMMARY=os.path.join(STATE, "builds", jid, f"summary-{i}.md"))
    p = None
    try:
        # A session of its own, so a timeout or a stop ends all of it.
        p = subprocess.Popen([CFG["bash"], "-c", cmd], cwd=tree, env=env, stdin=subprocess.DEVNULL, stdout=out,
                             stderr=subprocess.STDOUT, start_new_session=True)
        try:
            rc = p.wait(timeout=max(deadline - time.monotonic(), 0))
        except subprocess.TimeoutExpired:
            rc = None
        return 128 - rc if rc is not None and rc < 0 else rc
    finally:
        if p is not None and p.poll() is None:
            end_all(p)
        subprocess.run([*git, "worktree", "remove", "--force", tree], capture_output=True)
        shutil.rmtree(tree, ignore_errors=True)


def end_all(p):
    """Ends p's whole session: SIGTERM, then SIGKILL ten seconds on."""
    for sig, wait in ((signal.SIGTERM, 10), (signal.SIGKILL, None)):
        try:
            os.killpg(p.pid, sig)
        except ProcessLookupError:
            return
        try:
            p.wait(timeout=wait)
            return
        except subprocess.TimeoutExpired:
            continue


def sweep(checkout):
    """Worktrees under runsDir of runs that have ended (killed with no
    chance to remove their own), and what the checkout knows of them."""
    root = CFG["runsDir"]
    for d in os.listdir(root) if os.path.isdir(root) else ():
        jid = d.rsplit("-", 1)[0]
        if C.build_state(jid) not in (None, "running"):
            subprocess.run(["git", "-C", checkout, "worktree", "remove", "--force", os.path.join(root, d)], capture_output=True)
            shutil.rmtree(os.path.join(root, d), ignore_errors=True)
    subprocess.run(["git", "-C", checkout, "worktree", "prune"], capture_output=True)


def user_path():
    """The PATH the machine user's own units start with, its manager's (as
    proj's jobs get it), for a run's commands; None when systemd can't say,
    or says it escaped."""
    p = C.user_systemctl("show-environment")
    if p.returncode:
        return None
    paths = [line[len("PATH="):] for line in p.stdout.splitlines() if line.startswith("PATH=")]
    return paths[0] if paths and paths[0] and not paths[0].startswith("$") else None


def realise(jid):
    """The build's derivations built, inside its heavy slot (`autoland-steward
    realise ID`, run): one `nix-store --realise --add-root` each, all at
    once, so nix roots each one's outputs in the build's folder before its
    process ends and its temporary roots go. Nothing can collect a pass
    between its build and its verdict (one realise of them all would root
    none of them when any failed). How each ended goes in realised.json;
    the exit is the machine's failure when any had one, else the worst
    build failure, else 0."""
    b = load(f"builds/{jid}/build.json", None)
    here = os.path.join(STATE, "builds", jid)
    ps, stopped = {}, []

    def stop(*_):
        stopped.append(True)
        for p in ps.values():
            p.terminate()

    signal.signal(signal.SIGTERM, stop)
    for i, d in enumerate(sorted(set(b["drvs"].values()))):
        ps[d] = subprocess.Popen([f"{CFG['nix']}/nix-store", "--realise", "--add-root", os.path.join(here, f"out-{i}"),
                                  "--indirect", d], stdin=subprocess.DEVNULL)
    if stopped:  # stopped while they started: the late ones too
        stop()
    rcs = {d: 128 - rc if rc < 0 else rc for d, p in ps.items() for rc in [p.wait()]}
    save(f"builds/{jid}/realised.json", rcs)
    broke = [rc for rc in rcs.values() if rc and rc not in BUILD_FAILED]
    return broke[0] if broke else max(rcs.values(), default=0)


def judged(jid, b):
    """A done build's end: each check's verdict, from how its derivation's
    realise ended, naming it: a pass on 0, its outputs rooted already, a
    failure on a build failure's exit. Failed instead (built again) when a
    realise's end is missing, or the outputs of one that passed aren't
    valid (or the store can't say)."""
    rcs = load(f"builds/{jid}/realised.json", None) or {}
    if not all(d in rcs for d in b["drvs"].values()):
        log(f"build {jid}: how some of its derivations' builds ended is missing")
        return {"state": "failed"}
    passed = [c for c, d in b["drvs"].items() if rcs[d] == 0]
    if missing := unbuilt({o for c in passed for o in b["outs"][c]}):
        log(f"build {jid}: built, but not in the store: {', '.join(sorted(missing))}")
        return {"state": "failed"}
    return {"verdicts": {c: ["success", f"Built here and passed: {d}"] if c in passed
                         else ["failure", f"Built here and failed: {d}"] for c, d in b["drvs"].items()}}


# nix-store's exit codes for builds that failed: 100 and up, each bit of the
# rest a way one did (timed out, a hash that didn't match, not
# deterministic). Anything else, a signal's or heavy's own included, is the
# machine's failure, not a verdict.
BUILD_FAILED = range(100, 108)


def state_of(rc):
    """How a build ended, by nix-store's exit (None: stopped at proofTimeout)."""
    if rc is None:
        return "timed out"
    return "done" if rc == 0 or rc in BUILD_FAILED else "failed"


def gone(drvs):
    """Whether any of the derivations has been collected from the store (or
    the store can't say)."""
    return bool(drvs) and bool(unbuilt(drvs))


def verdicts(jid):
    """A done build's verdicts, by check ({check: (state, description)}), as
    it kept them when it ended (judged). None when it kept none."""
    said = (load(f"builds/{jid}/end.json", None) or {}).get("verdicts")
    if not said:
        log(f"build {jid} kept no verdicts")
        return None
    return {c: tuple(v) for c, v in said.items()}


def failed_log(jid, checks):
    """What goes on the PR for a build whose checks failed: which, and the
    end of its log, where a VM test says which subtest failed and how: its
    last LOG_TAIL lines, cut from the front to what fits in COMMENT_BYTES
    with the rest of the comment, so GitHub always takes it."""
    b = load(f"builds/{jid}/build.json", {})
    try:
        with open(os.path.join(STATE, "builds", jid, "log"), errors="replace") as f:
            tail = "".join(f.readlines()[-LOG_TAIL:])
    except OSError:
        tail = "(its log is gone)\n"
    fence = "~~~~"  # a log can hold backticks, not this
    names = ", ".join(f"`{c}`" for c in checks)
    drvs = ", ".join(f"`{b['drvs'][c]}`" for c in checks)
    top = (f"{names} failed on {b['head']}: building {drvs} on this machine failed. "
           f"The end of its log (`nix log` on the derivation has all of it):\n\n{fence}\n")
    log_bytes = tail.replace(fence, "~~~").encode()
    room = COMMENT_BYTES - len((top + fence + "\n").encode())
    if len(log_bytes) > room:
        # A character the cut lands inside is dropped whole.
        log_bytes = "…".encode() + log_bytes[-(room - len("…".encode())):].decode(errors="ignore").encode()
    return f"{top}{log_bytes.decode()}{fence}\n"


def note(jid, said):
    """What goes on the PR for a done proof, given its verdicts still to
    post (said), or None: a build's failed checks with its log's end
    (failed_log); a run's summaries and failures (ran)."""
    b = load(f"builds/{jid}/build.json", {})
    failed = sorted(c for c, (state, _) in said.items() if state == "failure")
    if "runs" in b:
        return ran(jid, b, said)
    return failed_log(jid, failed) if failed else None


def ran(jid, b, said):
    """A run's comment: for each of its checks in said, what its command
    wrote to PROOF_SUMMARY, and for a failure that wrote none, its exit.
    Never its log, which stays here: a command's output can hold what its
    repo keeps private, and only the summary is its own word on what's fit
    to post. None when nothing failed and nothing was written. Cut to fit
    COMMENT_BYTES, each summary its share of what's left."""
    exits = (load(f"builds/{jid}/end.json", None) or {}).get("exits", {})
    parts = []
    for i, c in enumerate(sorted(b["runs"])):
        if c not in said:
            continue
        try:
            with open(os.path.join(STATE, "builds", jid, f"summary-{i}.md"), errors="replace") as f:
                summary = f.read().strip()
        except OSError:
            summary = ""
        if said[c][0] == "failure":
            head = f"`{c}` failed on {b['head']}: it ran here and exited {exits.get(c)}."
            if not summary:
                head += f" What it printed stays on this machine, in the log of run `{jid}`."
        elif summary:
            head = f"`{c}` passed on {b['head']}."
        else:
            continue
        parts.append((head, summary))
    if not parts:
        return None
    room = (COMMENT_BYTES - sum(len(h.encode()) + 4 for h, _ in parts)) // len(parts)
    out = []
    for head, summary in parts:
        cut = summary.encode()
        if len(cut) > room:
            # A character the cut lands inside is dropped whole.
            cut = cut[:max(room - len("\n…".encode()), 0)].decode(errors="ignore").encode() + "\n…".encode()
        out.append(f"{head}\n\n{cut.decode()}" if summary else head)
    return "\n\n".join(out) + "\n"


def fits(st, stage):
    """Whether the PR's recorded proof is the one stage ([kind, checks,
    {check: attribute}]) wants: the same kind, with each of stage's checks
    among its own, from the same attribute. Stage has only the checks
    still to post, so one that's posted some is still the same proof. A
    proof recorded before proofs said what they were is matched on its kind
    alone."""
    jid = st.get("provedJob")
    was = st.get("provedFor") or ["build" if jid and jid.startswith(C.BUILD) else "job"]
    if was[0] != stage[0] or len(was) == 1:
        return was[0] == stage[0]
    return set(stage[1]) <= set(was[1]) and all(was[2].get(c) == stage[2].get(c) for c in stage[1])


def still(st, said, attrs):
    """Of the PR's recorded build's or run's verdicts (said), those its
    checks still want: each check's that's still built or run (attrs, the
    repo's proofBuilds and proofRuns) from the attribute or command that
    proof had. A check dropped or remapped since gets none of it."""
    was = st.get("provedFor") or []
    if len(was) != 3 or was[0] not in ("build", "run"):
        return {}
    return {c: v for c, v in said.items() if c in attrs and attrs[c] == was[2].get(c)}


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
