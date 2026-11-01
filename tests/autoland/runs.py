"""A proof check that's a command (proofRuns) is the steward's own to run,
with no Claude job, in the unit a build has: the command in bash, in a fresh
worktree of the repo's checkout at the PR's head, with the machine user's
own PATH and PROOF_* set, origin's base fetched. Its exit is the verdict: 0
a pass, 1 to 125 a failure, anything else the machine failing (run again,
never a verdict). What it prints stays in its log; only what it writes to
PROOF_SUMMARY goes on the PR. Runs come after builds and before Claude
jobs; a stop or a timeout ends the whole command and removes its worktree."""

import json
import os
import signal
import subprocess
import sys
import time

import proofs as P
from harness import CI_GREEN, KICKS, STATES, WROTE, JOBS, C, S, clean, fresh_pr_state, pr_state, run, sh, stand_in, tmp

stand_in()

# A repo on its "GitHub" (a bare repo with main and PR 5's head, as
# refs/pull/5/head), and the machine's checkout of it, origin and all.
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
origin, work, checkout = f"{tmp}/runs-origin.git", f"{tmp}/runs-work", f"{tmp}/runs-src/r"
sh("git", "init", "-q", "--bare", origin)
sh("git", "init", "-q", "-b", "main", work)


def commit(text, ref):
    with open(f"{work}/f", "w") as f:
        f.write(text)
    sh(*GIT, "add", "f", cwd=work)
    sh(*GIT, "commit", "-qm", text.strip(), cwd=work)
    sh("git", "push", "-q", origin, f"HEAD:{ref}", cwd=work)
    return sh("git", "rev-parse", "HEAD", cwd=work).strip()


commit("main\n", "refs/heads/main")
os.makedirs(os.path.dirname(checkout))
sh("git", "clone", "-q", origin, checkout)
# Then main moves on, and PR 5 is opened: a run fetches both.
commit("main moved on\n", "refs/heads/main")
HEAD = commit("pr\n", "refs/pull/5/head")

# Stand-ins for systemctl and systemd-run (each records how it was run),
# and a tool only the machine user's own PATH has.
bin_dir, user_bin, TOOLS, TOOL_LOG = f"{tmp}/runs-bin", f"{tmp}/runs-user-bin", f"{tmp}/runs-tools.json", f"{tmp}/runs-log"
os.makedirs(bin_dir)
os.makedirs(user_bin)
USER_PATH = f"{user_bin}:{os.environ['PATH']}"
for tool in ("systemctl", "systemd-run"):
    with open(f"{bin_dir}/{tool}", "w") as f:
        f.write(f"""#!{sys.executable}
import json, sys
a = sys.argv[1:]
with open({TOOL_LOG!r}, "a") as f:
    f.write(json.dumps({{"tool": {tool!r}, "args": a}}) + "\\n")
do = json.load(open({TOOLS!r})).get({tool!r}, {{}})
if a[:2] == ["--user", "show-environment"]:
    if do.get("env-rc"):
        sys.exit(do["env-rc"])
    print("HOME=/home/sam")
    print(do.get("path", "PATH=" + {USER_PATH!r}))
elif a[:2] == ["--user", "show"]:
    print(do.get("active", "inactive"))
else:
    sys.exit(do.get("rc", 0))
""")
    os.chmod(f"{bin_dir}/{tool}", 0o755)
with open(f"{user_bin}/users-tool", "w") as f:
    f.write("#!/bin/sh\necho the user\\'s own tool\n")
os.chmod(f"{user_bin}/users-tool", 0o755)


def tools(**do):
    json.dump(do, open(TOOLS, "w"))
    open(TOOL_LOG, "w").close()


def ran_tool(tool):
    return [x for x in map(json.loads, open(TOOL_LOG)) if x["tool"] == tool]


kept_path = os.environ["PATH"]
os.environ["PATH"] = f"{bin_dir}:{kept_path}"
RUNS = S.CFG["runsDir"]
os.makedirs(RUNS)
OUT = f"{tmp}/runs-out"  # what the commands say they saw


def made(runs, head=HEAD, n=5):
    """A run of runs ({check: command}), as start_run leaves its folder."""
    jid = f"{C.BUILD}sam-r-{n}-{head[:12]}-{time.time_ns()}"
    os.makedirs(f"{tmp}/state/builds/{jid}")
    C.save(f"builds/{jid}/build.json", {"repo": "sam/r", "number": n, "head": head, "base": "main",
                                        "checkout": checkout, "runs": runs})
    return jid


def end(jid):
    return json.load(open(f"{tmp}/state/builds/{jid}/end.json"))


def out(name):
    with open(f"{OUT}/{name}") as f:
        return f.read()


def worktrees():
    return [x for x in sh("git", "-C", checkout, "worktree", "list", "--porcelain").splitlines() if x.startswith("worktree ")]


def gone(pid):
    """A process that's ended (or is only a zombie nobody reaped)."""
    try:
        with open(f"/proc/{pid}/status") as f:
            return "\nState:\tZ" in f.read()
    except FileNotFoundError:
        return True


def fresh():
    fresh_pr_state()
    tools()
    subprocess.run(["rm", "-rf", OUT])
    os.makedirs(OUT)


# A pass: the command in a worktree at the head, with the user's PATH, the
# PROOF_* names and origin's base fetched; the steward's own config isn't
# the command's. Its verdict, its exit, its log and its kick; its worktree
# gone.
fresh()
SEEN = (f"pwd >{OUT}/pwd; git rev-parse HEAD >{OUT}/head; env | grep -E '^(PROOF_|AUTOLAND_)' | sort >{OUT}/env; "
        f"users-tool >{OUT}/tool; git show origin/main:f >{OUT}/main; echo SECRET-LOG-LINE")
jid = made({"gate": SEEN})
KICKS.clear()
P.run(jid)
assert end(jid) | {"at": 0} == {"state": "done", "exit": 0, "verdicts": {"gate": ["success", "Ran here and passed"]},
                                "exits": {"gate": 0}, "at": 0}, end(jid)
assert KICKS == ["/kick/autoland/sam/r"], KICKS
assert out("pwd").strip() == f"{RUNS}/{jid}-0" and out("head").strip() == HEAD, (out("pwd"), out("head"))
assert out("env").splitlines() == ["PROOF_BASE=main", "PROOF_CHECK=gate", f"PROOF_HEAD={HEAD}", "PROOF_NUMBER=5",
                                   "PROOF_REPO=sam/r", f"PROOF_SUMMARY={tmp}/state/builds/{jid}/summary-0.md"], out("env")
assert out("tool") == "the user's own tool\n" and out("main") == "main moved on\n", (out("tool"), out("main"))
log = open(f"{tmp}/state/builds/{jid}/log").read()
assert "SECRET-LOG-LINE" in log and "autoland: gate: exit 0" in log, log
assert os.listdir(RUNS) == [] and worktrees() == [f"worktree {checkout}"], (os.listdir(RUNS), worktrees())
assert C.build_state(jid) == "done"
# Its exit says the verdict: 1 to 125 a failure, saying so; a signal, a
# shell's 128 and up, a command not found or not runnable is the machine's
# failure, no verdict.
for cmd, state in (("exit 1", "done"), ("exit 125", "done"), ("exit 126", "failed"), ("exit 127", "failed"),
                   ("no-such-command", "failed"), ("exit 200", "failed"), ("kill -9 $$", "failed")):
    fresh()
    jid = made({"gate": cmd})
    P.run(jid)
    assert end(jid)["state"] == state, (cmd, end(jid))
    if state == "done":
        rc = int(cmd.split()[1])
        assert end(jid)["verdicts"] == {"gate": ["failure", f"Ran here and failed: exit {rc}"]}, end(jid)
    else:
        assert "verdicts" not in end(jid), end(jid)
    assert os.listdir(RUNS) == [] and len(worktrees()) == 1, (cmd, os.listdir(RUNS), worktrees())
# Each check's command in turn, in a worktree of its own, by name; one the
# machine failed ends the run there, with no verdict for any.
fresh()
jid = made({"b": f"echo b >>{OUT}/order; touch made-by-b", "a": f"echo a >>{OUT}/order; test ! -e made-by-b"})
P.run(jid)
assert out("order") == "a\nb\n" and end(jid)["verdicts"] == {"a": ["success", "Ran here and passed"],
                                                            "b": ["success", "Ran here and passed"]}, end(jid)
fresh()
jid = made({"a": "kill -9 $$", "b": f"touch {OUT}/b"})
P.run(jid)
assert end(jid)["state"] == "failed" and not os.path.exists(f"{OUT}/b"), end(jid)
# A head that can't be fetched runs nothing: the machine's failure.
fresh()
jid = made({"gate": f"touch {OUT}/ran"}, head="b" * 40)
P.run(jid)
assert end(jid)["state"] == "failed" and not os.path.exists(f"{OUT}/ran"), end(jid)
assert f"can't fetch {'b' * 40}" in open(f"{tmp}/state/builds/{jid}/log").read()
# Nor does one whose user's PATH systemd can't say, or says escaped.
for do in ({"env-rc": 1}, {"path": "PATH=$'/odd\\npath'"}, {"path": "LANG=C"}):
    fresh()
    tools(systemctl=do)
    jid = made({"gate": f"touch {OUT}/ran"})
    P.run(jid)
    assert end(jid)["state"] == "failed" and not os.path.exists(f"{OUT}/ran"), (do, end(jid))
assert ran_tool("systemctl")[0]["args"] == ["--user", "show-environment"], ran_tool("systemctl")
# Past proofTimeout, all of it ends, what it started too, and says so.
fresh()
S.CFG["proofTimeout"] = 0.02  # minutes: 1.2 s
jid = made({"gate": f"sleep 60 & echo $! >{OUT}/child; wait"})
began = time.time()
P.run(jid)
assert end(jid)["state"] == "timed out" and time.time() - began < 20, end(jid)
assert gone(int(out("child"))) and os.listdir(RUNS) == [], out("child")
S.CFG["proofTimeout"] = 180
# Stopped (its unit's SIGTERM): the command and what it started end, its
# worktree goes, and it ended short.
fresh()
jid = made({"gate": f"sleep 60 & echo $! >{OUT}/child; wait"})
p = subprocess.Popen(P.own("build", jid))
for _ in range(200):
    if os.path.exists(f"{OUT}/child") and out("child").strip():
        break
    time.sleep(0.05)
p.send_signal(signal.SIGTERM)
assert p.wait(timeout=30) == 0, p.returncode
assert end(jid)["state"] == "failed" and gone(int(out("child"))), end(jid)
assert os.listdir(RUNS) == [] and len(worktrees()) == 1, (os.listdir(RUNS), worktrees())
# A run killed outright leaves its worktree: the next run's sweep removes
# it once its run has ended, and the checkout forgets it; one still running
# is left alone.
fresh()
dead, alive = made({"gate": "true"}), made({"gate": "true"})
for j in (dead, alive):
    sh("git", "-C", checkout, "worktree", "add", "-q", "--detach", f"{RUNS}/{j}-0", HEAD)
C.save(f"builds/{dead}/end.json", {"state": "failed", "exit": None, "at": time.time()})
tools(systemctl={"active": "active"})  # alive's unit is still running
jid = made({"gate": "true"})
P.run(jid)
assert os.listdir(RUNS) == [f"{alive}-0"], os.listdir(RUNS)
assert worktrees() == [f"worktree {checkout}", f"worktree {RUNS}/{alive}-0"], worktrees()
sh("git", "-C", checkout, "worktree", "remove", "--force", f"{RUNS}/{alive}-0")
print("proof runs' run: ok")

# Starting one: what it runs, from where, in a unit that runs this steward;
# none without a checkout, or when systemd won't.
fresh()
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "make gate", "other": "make other"}}
kept_project_dir = C.project_dir
C.project_dir = lambda repo: checkout if repo == "sam/r" else None
pr = clean()
jid = P.start_run("sam/r", pr, ["gate"], {})
assert jid and jid.startswith(C.BUILD + "sam-r-5-" + "a" * 12), jid
assert json.load(open(f"{tmp}/state/builds/{jid}/build.json")) == {
    "repo": "sam/r", "number": 5, "head": "a" * 40, "base": "main", "checkout": checkout, "runs": {"gate": "make gate"}}
a = ran_tool("systemd-run")[0]["args"]
assert a[:4] == ["--user", "--quiet", "--collect", f"--unit={C.build_unit(jid)}"] and a[a.index("--") + 1:] == P.own("build", jid), a
tools(**{"systemd-run": {"rc": 1}})
assert P.start_run("sam/r", pr, ["gate"], {}) is None
C.project_dir = lambda repo: None
tools()
assert P.start_run("sam/r", pr, ["gate"], {}) is None and ran_tool("systemd-run") == []
C.project_dir = kept_project_dir
print("proof runs' start: ok")

# A pass over a PR whose gate is a run: no Claude job, a run; its verdict
# posted once it's done, and only its summary on the PR, never its log.
GATE = CI_GREEN + [{"__typename": "StatusContext", "context": "gate", "state": "PENDING", "description": ""}]
STARTED = []
kept_start, kept_build, kept_gh = P.start_run, P.build, C.gh


def start_run(repo, pr, checks, st):
    STARTED.append(made({c: C.repo_cfg(repo)["proofRuns"][c] for c in checks}, pr["headRefOid"], pr["number"]))
    return STARTED[-1]


def finish(jid):
    P.run(jid)
    STATES[jid] = end(jid)["state"]


def gh(*a, **k):
    """GitHub's word on the head's statuses: none settled yet."""
    if a[:1] == ("api",) and a[1].endswith("/status?per_page=100"):
        return {"statuses": []}
    return kept_gh(*a, **k)


def statuses(context="gate"):
    return [dict(x.split("=", 1) for x in w.split(" -f ")[1:]) for w in WROTE if "/statuses/" in w and f"context={context}" in w]


def comments():
    return [w for w in WROTE if "/comments" in w]


P.start_run, C.gh = start_run, gh
# The PR's head is HEAD here, so the runs fetch it for real.
for cmd, summary, state in (("echo SECRET-LOG-LINE", None, "success"),
                            ("echo SECRET-LOG-LINE; echo '## Receipt: 6 of 6 passed' >\"$PROOF_SUMMARY\"", "## Receipt: 6 of 6 passed", "success"),
                            ("echo SECRET-LOG-LINE; exit 3", None, "failure"),
                            ("echo SECRET-LOG-LINE; echo 'clippy: exit 101' >\"$PROOF_SUMMARY\"; exit 1", "clippy: exit 101", "failure")):
    fresh()
    STARTED.clear()
    STATES.clear()
    S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": cmd}}
    status, _ = run(clean(sha=HEAD, checks=GATE), 700)
    assert JOBS == [] and len(STARTED) == 1 and "Settled; running gate here" in status, (status, JOBS, STARTED)
    assert pr_state()["provedFor"] == ["run", ["gate"], {"gate": cmd}], pr_state()
    # Still running: it says so, and nothing more starts.
    status, _ = run(clean(sha=HEAD, checks=GATE), 800)
    assert len(STARTED) == 1 and "Settled; running gate here" in status, status
    finish(STARTED[0])
    status, _ = run(clean(sha=HEAD, checks=GATE), 900)
    rc = end(STARTED[0])["exits"]["gate"]
    desc = "Ran here and passed" if state == "success" else f"Ran here and failed: exit {rc}"
    assert statuses() == [{"state": state, "context": "gate", "description": desc, "target_url": "u"}], WROTE
    assert "Settled; gate ran here, its verdict posted" in status, status
    said = comments()
    assert all("SECRET-LOG-LINE" not in c for c in said), said
    if state == "success" and not summary:
        assert said == [], said
    else:
        assert len(said) == 1, said
        # The comment goes up before the status.
        assert WROTE.index(said[0]) < WROTE.index(next(w for w in WROTE if "/statuses/" in w)), WROTE
        if state == "success":
            assert f"`gate` passed on {HEAD}." in said[0] and summary in said[0], said
        else:
            assert f"`gate` failed on {HEAD}: it ran here and exited {rc}." in said[0], said
            assert (summary in said[0]) if summary else (f"in the log of run `{STARTED[0]}`" in said[0]), said
    run(clean(sha=HEAD, checks=GATE), 1000)
    assert comments() == [], WROTE
# One the machine failed (killed): no verdict, and another runs it later.
fresh()
STARTED.clear()
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "kill -9 $$"}}
run(clean(sha=HEAD, checks=GATE), 700)
finish(STARTED[0])
run(clean(sha=HEAD, checks=GATE), 900)
status, _ = run(clean(sha=HEAD, checks=GATE), 960)
assert statuses() == [] and "Settled; the last run of gate crashed, so another starts at" in status, (status, WROTE)
status, _ = run(clean(sha=HEAD, checks=GATE), 1100)
assert len(STARTED) == 2 and "Settled; running gate here again (the last one crashed)" in status, (status, STARTED)
assert statuses() == [] and JOBS == [], WROTE
# One that can't start says so.
fresh()
P.start_run = lambda repo, pr, checks, st: None
status, _ = run(clean(sha=HEAD, checks=GATE), 700)
assert "Settled, but the run of gate couldn't start" in status and JOBS == [], status
P.start_run = start_run
# Builds go first, then runs, then Claude jobs; each starts afresh, not as
# the last one's second go.
BUILT = []


def build(repo, pr, checks, st):
    BUILT.append(f"{C.BUILD}built-{len(BUILT)}")
    return BUILT[-1]


P.build = build
kept_outputs = P.outputs
P.outputs = lambda repo, pr, checks, st: ["/nix/store/" + "x" * 32 + "-vm"] if checks == ["vm"] else None
fresh()
STARTED.clear()
S.CFG["repos"]["sam/r"] = {"proofBuilds": {"vm": "checks.x86_64-linux.vm"}, "proofRuns": {"gate": "true"},
                           "proofJobs": {"other": "Run the other proof."}}


def pending(*names, passed=()):
    return CI_GREEN + [{"__typename": "StatusContext", "context": c, "state": "SUCCESS" if c in passed else "PENDING",
                        "description": ""} for c in names]


status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other")), 700)
assert BUILT == [f"{C.BUILD}built-0"] and STARTED == [] and JOBS == [] and "building vm here" in status, (status, BUILT, STARTED)
STATES[BUILT[0]] = "done"
status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other", passed=["vm"])), 900)
assert len(STARTED) == 1 and JOBS == [] and "Settled; running gate here" in status and "again" not in status, (status, JOBS)
finish(STARTED[0])
status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other", passed=["vm", "gate"])), 1100)
assert len(JOBS) == 1 and "Run the other proof." in JOBS[0] and "again" not in status, (status, JOBS)
assert "provedAgain" not in pr_state(), pr_state()
P.build, P.outputs = kept_build, kept_outputs
# A run's command remapped while it's pending is run again: the old one
# says nothing of the new; dropped since, its verdict left unposted isn't
# posted.
assert P.still({"provedFor": ["run", ["gate"], {"gate": "make gate"}]}, {"gate": ("success", "x")},
               {"gate": "make gate"}) == {"gate": ("success", "x")}
assert P.still({"provedFor": ["run", ["gate"], {"gate": "make gate"}]}, {"gate": ("success", "x")},
               {"gate": "make gate --fast"}) == {}
fresh()
STARTED.clear()
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "true"}}
run(clean(sha=HEAD, checks=GATE), 700)
finish(STARTED[0])
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "true --again"}}
status, _ = run(clean(sha=HEAD, checks=GATE), 900)
assert len(STARTED) == 2 and statuses() == [] and "running gate here" in status, (status, STARTED, WROTE)
# What a fix job is told of a run: the command, where it runs, and that the
# steward posts its status. A check in proofBuilds is the build, one in
# proofRuns and proofJobs the run, however each spells it.
rc = {"proofJobs": {"GATE": "Run it.", "x": "Run x."}, "proofBuilds": {"vm": "checks.x86_64-linux.vm"},
      "proofRuns": {"gate": "make gate 'quoted'", "VM": "not this"}}
told = C.machine(rc)
assert told.keys() == {"vm", "gate", "x"}, told
assert "bash -c 'make gate '\"'\"'quoted'\"'\"''" in told["gate"] and "never post that status yourself" in told["gate"], told
assert told["vm"].startswith("`nix build .#checks.x86_64-linux.vm -L`") and told["x"] == "Run x.", told
# A summary too big for one comment between them is cut to fit, its start
# kept, and the comment always short of GitHub's limit.
fresh()
jid = made({"a": "true", "b": "true"})
for i in (0, 1):
    with open(f"{tmp}/state/builds/{jid}/summary-{i}.md", "w") as f:
        f.write("界" * 40000)
said = P.note(jid, {"a": ("failure", ""), "b": ("success", "")})
assert len(said.encode()) <= P.COMMENT_BYTES and said.count("…") == 2 and "`a` failed on" in said, len(said.encode())
print("proof runs' verdicts: ok")

P.start_run, P.build, C.gh = kept_start, kept_build, kept_gh
del S.CFG["repos"]["sam/r"]
os.environ["PATH"] = kept_path
STATES.clear()
