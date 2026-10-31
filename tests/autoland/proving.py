"""A proof check that's a build (proofBuilds) is the steward's own to build,
with no Claude job: a unit of the machine user's that runs `nix-store
--realise --keep-going` on its derivations (heavy-slotted where the machine
has heavy), its log beside them and how it ended in end.json, then kicks its
repo's pass. That pass posts each check as its outputs are valid or not,
naming its derivation, and a failure's log tail goes on the PR once. A build
that stopped short (killed, nix couldn't build at all, past proofTimeout) is
the machine failing: built again later, never a failure on the PR. It's
asked after and stopped like a job (common.job_state, stop_job)."""

import json
import os
import subprocess
import sys
import time

import proofs as P
from harness import CI_GREEN, FAIL, T0, JOBS, KICKS, REAL, STATES, STOPPED, WROTE, C, S, clean, fresh_pr_state, pr_state, run, stand_in, tmp

stand_in()

# Stand-ins for nix-store, systemd-run and systemctl: each records how it
# was run (and the env it was given), then does what TOOLS says.
bin_dir, TOOLS, TOOL_LOG = f"{tmp}/proving-bin", f"{tmp}/proving-tools.json", f"{tmp}/proving-log"
os.makedirs(bin_dir)
for tool in ("nix-store", "systemd-run", "systemctl"):
    with open(f"{bin_dir}/{tool}", "w") as f:
        f.write(f"""#!{sys.executable}
import json, os, sys, time
a = sys.argv[1:]
with open({TOOL_LOG!r}, "a") as f:
    f.write(json.dumps({{"tool": {tool!r}, "args": a, "env": {{k: os.environ.get(k) for k in ("AUTOLAND_CONFIG", "XDG_RUNTIME_DIR")}}}}) + "\\n")
do = json.load(open({TOOLS!r})).get({tool!r}, {{}})
if {tool!r} == "nix-store" and a[:1] == ["--query"]:
    print("\\n".join(d.removesuffix(".drv") for d in a[2:]))
elif {tool!r} == "nix-store" and a[:1] == ["--check-validity"]:
    # A derivation is in the store until it's collected; an output once valid.
    print("\\n".join(p for p in a[2:] if ((p in do.get("collected", [])) if p.endswith(".drv") else (p not in do.get("valid", [])))))
elif {tool!r} == "nix-store":
    if do.get("kill"):
        os.kill(os.getpid(), 9)
    print("building", *a[2:], file=sys.stderr)
    print("\\n".join(do.get("log", [])))
    time.sleep(do.get("sleep", 0))
    sys.exit(do.get("rc", 0))
elif {tool!r} == "systemctl" and a[:2] == ["--user", "show"]:
    if do.get("rc"):
        sys.exit(do["rc"])
    print(do.get("active", "inactive"))
else:
    sys.exit(do.get("rc", 0))
""")
    os.chmod(f"{bin_dir}/{tool}", 0o755)


def tools(**do):
    json.dump(do, open(TOOLS, "w"))
    open(TOOL_LOG, "w").close()


def ran(tool):
    return [x for x in map(json.loads, open(TOOL_LOG)) if x["tool"] == tool]


kept_path, kept_nix = os.environ["PATH"], S.CFG["nix"]
os.environ["PATH"] = f"{bin_dir}:{kept_path}"
S.CFG["nix"] = bin_dir
X, Y = "/nix/store/" + "x" * 32 + "-vm-test-run-vm", "/nix/store/" + "y" * 32 + "-vm-test-run-other"


def made(drvs, head="a" * 40, n=5):
    """A build of drvs ({check: drv}), as build() leaves its folder."""
    jid = f"{C.BUILD}sam-r-{n}-{head[:12]}-{time.time_ns()}"
    os.makedirs(f"{tmp}/state/builds/{jid}")
    C.save(f"builds/{jid}/build.json", {"repo": "sam/r", "number": n, "head": head, "drvs": drvs})
    return jid


def end(jid):
    return json.load(open(f"{tmp}/state/builds/{jid}/end.json"))


# The run: every derivation at once, keeping going past a failure, with its
# log beside it, how it ended, and its repo's pass kicked.
fresh_pr_state()
# Killed (no exit status of its own), a signal's or a missing command's
# status from a shell (137, 127), or nix failing outright (1) is the
# machine's failure; 100 to 107 is builds that failed, each bit a way.
for rc, state in ((0, "done"), (100, "done"), (101, "done"), (104, "done"), (107, "done"), (108, "failed"),
                  (1, "failed"), (127, "failed"), (137, "failed"), ("kill", "failed")):
    tools(**{"nix-store": {"rc": rc, "log": ["subtest: it boots", "ok"]} if rc != "kill" else {"kill": True}})
    jid = made({"vm": X + ".drv", "other": Y + ".drv"})
    KICKS.clear()
    P.run(jid)
    assert end(jid)["state"] == state, (rc, end(jid))
    assert KICKS == ["/kick/autoland/sam/r"], KICKS
    realised = [x["args"] for x in ran("nix-store") if x["args"][:1] == ["--realise"]]
    assert realised == [["--realise", "--keep-going", "--add-root", f"{tmp}/state/builds/{jid}/out", "--indirect",
                         X + ".drv", Y + ".drv"]], ran("nix-store")
    if rc != "kill":
        log = open(f"{tmp}/state/builds/{jid}/log").read()
        assert "subtest: it boots" in log and f"{X}.drv" in log, log
    assert C.build_state(jid) == state, jid
# In a heavy slot, where the machine has heavy.
with open(f"{bin_dir}/heavy", "w") as f:
    f.write(f"#!/bin/sh\necho heavy \"$@\" >>{tmp}/heavy-log\n[ \"$1\" = -- ] && shift\nexec \"$@\"\n")
os.chmod(f"{bin_dir}/heavy", 0o755)
S.CFG["heavy"] = f"{bin_dir}/heavy"
tools()
jid = made({"vm": X + ".drv"})
P.run(jid)
assert open(f"{tmp}/heavy-log").read().split() == ["heavy", "--", f"{bin_dir}/nix-store", "--realise", "--keep-going",
                                                   "--add-root", f"{tmp}/state/builds/{jid}/out", "--indirect", X + ".drv"]
assert end(jid)["state"] == "done", end(jid)
# Under a heavy that waits on it, nix killed comes back as the shell's 137:
# the machine's failure still, not a verdict.
with open(f"{bin_dir}/heavy", "w") as f:
    f.write("#!/bin/sh\n[ \"$1\" = -- ] && shift\n\"$@\"\nexit $?\n")
tools(**{"nix-store": {"kill": True}})
jid = made({"vm": X + ".drv"})
P.run(jid)
assert end(jid) | {"at": 0} == {"state": "failed", "exit": 137, "at": 0}, end(jid)
S.CFG["heavy"] = None
# Its derivations collected since they were worked out: nothing is built,
# and it ended short (a verdict needs them worked out again).
tools(**{"nix-store": {"collected": [Y + ".drv"]}})
jid = made({"vm": X + ".drv", "other": Y + ".drv"})
P.run(jid)
assert end(jid)["state"] == "failed" and not [x for x in ran("nix-store") if x["args"][:1] == ["--realise"]], ran("nix-store")
# An ended build's folder goes KEEP days on, a running one's never.
old_end = made({"vm": X + ".drv"})
C.save(f"builds/{old_end}/end.json", {"state": "done", "exit": 0, "at": time.time() - P.KEEP * 86400 - 60})
young, running_one = made({"vm": X + ".drv"}), made({"vm": X + ".drv"})
C.save(f"builds/{young}/end.json", {"state": "done", "exit": 0, "at": time.time() - 60})
P.prune()
assert not os.path.exists(f"{tmp}/state/builds/{old_end}"), old_end
assert os.path.isdir(f"{tmp}/state/builds/{young}") and os.path.isdir(f"{tmp}/state/builds/{running_one}")
# Past proofTimeout it's stopped, and says so.
S.CFG["proofTimeout"] = 0.02  # minutes: 1.2 s
tools(**{"nix-store": {"sleep": 30}})
jid = made({"vm": X + ".drv"})
began = time.time()
P.run(jid)
assert end(jid)["state"] == "timed out" and time.time() - began < 20, end(jid)
S.CFG["proofTimeout"] = 180
print("proof builds' run: ok")

# Its state while it has no end: running while its unit is, failed once
# it's gone without one, unknown when systemd can't say.
for active, state in (("active", "running"), ("activating", "running"), ("inactive", "failed"), ("failed", "failed")):
    tools(systemctl={"active": active})
    assert C.build_state(made({"vm": X + ".drv"})) == state, (active, state)
tools(systemctl={"rc": 1})
assert C.build_state(made({"vm": X + ".drv"})) is None
assert ran("systemctl")[0]["args"][:4] == ["--user", "show", "-P", "ActiveState"], ran("systemctl")
assert ran("systemctl")[0]["env"]["XDG_RUNTIME_DIR"] == f"/run/user/{os.getuid()}", ran("systemctl")
# Stopped like a job: its unit is, and it's over once systemd says so.
jid = made({"vm": X + ".drv"})
tools(systemctl={"active": "inactive"})
assert REAL["stop_job"](jid) and ran("systemctl")[0]["args"] == ["--user", "stop", C.build_unit(jid)], ran("systemctl")
tools(systemctl={"active": "active"})
assert not REAL["stop_job"](jid)
print("proof builds' state: ok")

# Starting one: its folder, then its unit, which runs this steward with
# this config; None when systemd won't.
fresh_pr_state()
tools()
st = {"proofOutputs": {"drvs": {"vm": X + ".drv", "other": Y + ".drv"}}}
pr = clean()
jid = P.build("sam/r", pr, ["vm"], st)
assert jid and jid.startswith(C.BUILD + "sam-r-5-" + "a" * 12), jid
assert json.load(open(f"{tmp}/state/builds/{jid}/build.json")) == {"repo": "sam/r", "number": 5, "head": "a" * 40,
                                                                   "drvs": {"vm": X + ".drv"}}
started = ran("systemd-run")[0]
a = started["args"]
assert a[:4] == ["--user", "--quiet", "--collect", f"--unit={C.build_unit(jid)}"], a
assert f"--setenv=AUTOLAND_CONFIG={os.environ['AUTOLAND_CONFIG']}" in a, a
assert a[a.index("--") + 1:][-2:] == ["build", jid] and a[a.index("--") + 1] == sys.executable, a
tools(**{"systemd-run": {"rc": 1}})
assert P.build("sam/r", pr, ["vm"], st) is None
# And `autoland-steward build ID` is that run.
tools()
jid = made({"vm": X + ".drv"})
json.dump({**json.load(open(os.environ["AUTOLAND_CONFIG"])), "nix": bin_dir}, open(f"{tmp}/proving-config.json", "w"))
p = subprocess.run([sys.executable, "-E", "-s", os.path.join(os.path.dirname(P.__file__), "steward.py"), "build", jid],
                   env={**os.environ, "AUTOLAND_CONFIG": f"{tmp}/proving-config.json"}, capture_output=True, text=True)
assert p.returncode == 0 and end(jid)["state"] == "done", p.stderr
assert not os.path.exists(f"{tmp}/state/pass.lock"), "a build takes no pass's lock"
print("proof builds' start: ok")

# A pass over a PR whose vm check is a build: no Claude job, a build.
S.CFG["repos"]["sam/r"] = {"proofBuilds": {"vm": "checks.x86_64-linux.vm"}}
VM = CI_GREEN + [{"__typename": "StatusContext", "context": "vm", "state": "PENDING", "description": ""}]
BUILT = []
kept_outputs, kept_build = P.outputs, P.build


def outputs(repo, pr, checks, st):
    st["proofOutputs"] = {"for": None, "paths": [X], "drvs": {"vm": X + ".drv"}, "at": C.NOW}
    return [X]


def build(repo, pr, checks, st):
    BUILT.append(made({c: st["proofOutputs"]["drvs"][c] for c in checks}, pr["headRefOid"], pr["number"]))
    return BUILT[-1]


P.outputs, P.build = outputs, build
kept_gh = C.gh


def gh(*a, **k):
    """GitHub's word on the head's statuses: none settled yet."""
    if a[:1] == ("api",) and a[1].endswith("/status?per_page=100"):
        return {"statuses": []}
    return kept_gh(*a, **k)


C.gh = gh
fresh_pr_state()
tools()
status, _ = run(clean(checks=VM), 700)
assert JOBS == [] and len(BUILT) == 1 and "Settled; building vm here" in status, (status, JOBS, BUILT)
assert pr_state()["provedJob"] == BUILT[0], pr_state()
# Still building: it says so, and nothing more starts.
status, _ = run(clean(checks=VM), 800)
assert len(BUILT) == 1 and "Settled; building vm here" in status, status


def statuses(context="vm"):
    return [dict(x.split("=", 1) for x in w.split(" -f ")[1:]) for w in WROTE if "/statuses/" in w and f"context={context}" in w]


def comments():
    return [w for w in WROTE if "/comments" in w]


# Done, its outputs valid: a pass naming its derivation, and no comment.
STATES[BUILT[0]] = "done"
tools(**{"nix-store": {"valid": [X]}})
status, _ = run(clean(checks=VM), 900)
assert statuses() == [{"state": "success", "context": "vm", "description": f"Built here and passed: {X}.drv", "target_url": "u"}], WROTE
assert comments() == [] and "Settled; vm built here, its verdict posted" in status, (status, WROTE)
# Done, not valid: a failure, and its log's end on the PR, once.
fresh_pr_state()
BUILT.clear()
tools()  # nothing built yet
status, _ = run(clean(checks=VM), 700)
assert len(BUILT) == 1, (status, WROTE)
with open(f"{tmp}/state/builds/{BUILT[0]}/log", "w") as f:
    f.write("".join(f"line {i}\n" for i in range(100)) + "subtest: it boots ... failed: ~~~~ no banner\n")
STATES[BUILT[0]] = "done"
tools(**{"nix-store": {"valid": []}})
run(clean(checks=VM), 900)
assert statuses() == [{"state": "failure", "context": "vm", "description": f"Built here and failed: {X}.drv", "target_url": "u"}], WROTE
said = comments()
assert len(said) == 1 and "no banner" in said[0] and "\nline 41\n" in said[0] and "\nline 40\n" not in said[0], said
assert "~~~~ no banner" not in said[0] and said[0].count("~~~~") == 2, said  # the fence holds
assert f"`vm` failed on {'a' * 40}" in said[0] and f"`{X}.drv`" in said[0], said
run(clean(checks=VM), 1000)
assert comments() == [], WROTE
# GitHub doesn't take the log: no status either, so the PR isn't handed to
# a fix without it; both go up on the next pass, the log once.
fresh_pr_state()
BUILT.clear()
tools()
run(clean(checks=VM), 700)
STATES[BUILT[0]] = "done"
FAIL.append("/comments")
status, _ = run(clean(checks=VM), 900)
FAIL.remove("/comments")
assert statuses() == [] and pr_state()["strikes"] == {"status": 1} and "its verdict not posted yet" in status, (status, WROTE)
run(clean(checks=VM), 1000)
assert len(comments()) == 1 and [s["state"] for s in statuses()] == ["failure"] and not pr_state()["strikes"], WROTE
# The store can't say: nothing posted, a strike, tried again next pass.
fresh_pr_state()
BUILT.clear()
tools()  # nothing built yet
run(clean(checks=VM), 700)
STATES[BUILT[0]] = "done"
os.rename(f"{bin_dir}/nix-store", f"{bin_dir}/nix-store.real")
status, _ = run(clean(checks=VM), 900)
os.rename(f"{bin_dir}/nix-store.real", f"{bin_dir}/nix-store")
assert statuses() == [] and pr_state()["strikes"] == {"status": 1} and "its verdict not posted yet" in status, (status, WROTE)
tools(**{"nix-store": {"valid": [X]}})
run(clean(checks=VM), 1000)
assert [s["state"] for s in statuses()] == ["success"] and not pr_state()["strikes"], WROTE
# A build that stopped short is the machine's failure: no verdict, and
# another builds it later.
fresh_pr_state()
BUILT.clear()
tools()  # nothing built yet
run(clean(checks=VM), 700)
STATES[BUILT[0]] = "failed"
run(clean(checks=VM), 900)
status, _ = run(clean(checks=VM), 960)
assert statuses() == [] and "Settled; the last build of vm crashed, so another starts at" in status, (status, BUILT)
status, _ = run(clean(checks=VM), 1100)
assert len(BUILT) == 2 and "Settled; building vm here again (the last one crashed)" in status, (status, BUILT)
assert statuses() == [] and JOBS == [], WROTE
# Its deadline is proofTimeout's, not a Claude job's.
assert min(C.WAKE.values()) >= C.NOW + S.CFG["proofTimeout"] * 60, C.WAKE
# A Claude job from before its check had a build, on this head: stopped
# while it's at work, and a build starts afresh, the job's lost answer and
# retries not carried over; one that ended without its verdict, the same.
for job_state, stopped in (("running", ["job-old"]), ("done", [])):
    fresh_pr_state()
    BUILT.clear()
    STOPPED.clear()
    tools()
    STATES["job-old"] = job_state
    C.save("pr-sam_r-5.json", {"head": "a" * 40, "seen": C.ts(T0), "proved": "a" * 40, "provedJob": "job-old", "provedAgain": "a" * 40,
                               "provedLost": "a" * 40, "provedAt": C.NOW})
    status, _ = run(clean(checks=VM), 700)
    assert STOPPED == stopped and len(BUILT) == 1 and "Settled; building vm here" in status, (job_state, status, STOPPED)
    assert "provedAgain" not in pr_state() and "again" not in status, pr_state()
# The Claude jobs after a build start afresh too: the build's done isn't a
# job that lost its answer, and their first is no second.
fresh_pr_state()
BUILT.clear()
S.CFG["repos"]["sam/r"] = {"proofBuilds": {"vm": "checks.x86_64-linux.vm"}, "proofJobs": {"other": "Run the other proof."}}
OTHER = CI_GREEN + [{"__typename": "StatusContext", "context": "vm", "state": "SUCCESS", "description": ""},
                    {"__typename": "StatusContext", "context": "other", "state": "PENDING", "description": ""}]
C.save("pr-sam_r-5.json", {"head": "a" * 40, "seen": C.ts(T0), "proved": "a" * 40, "provedJob": made({"vm": X + ".drv"}), "provedAt": C.NOW})
STATES[pr_state()["provedJob"]] = "done"
status, _ = run(clean(checks=OTHER), 700)
assert len(JOBS) == 1 and "Run the other proof." in JOBS[0] and "again" not in status, (status, JOBS)
assert "provedAgain" not in pr_state() and not pr_state()["provedJob"].startswith(C.BUILD), pr_state()
S.CFG["repos"]["sam/r"] = {"proofBuilds": {"vm": "checks.x86_64-linux.vm"}}
print("proof builds' verdicts: ok")

P.outputs, P.build, C.gh = kept_outputs, kept_build, kept_gh
del S.CFG["repos"]["sam/r"]
os.environ["PATH"], S.CFG["nix"] = kept_path, kept_nix
STATES.clear()
