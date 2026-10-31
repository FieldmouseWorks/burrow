"""Renovate's runs (renovate-run), with stand-ins for gh, gh-token, Renovate
and its autodiscoverFilter: which repos get a run, with whose token. As the
user, gh's login covers every repo in one run; as the machine's own App, each
repo gets a run of its own with a token of its owner's installation, asked
for that run once it holds the runs' lock. Each run that ends, failed or not,
is noted for its repo (or `all`), the last ten.

    python3 -I tests/renovate-run.py modules/updates/renovate.sh
"""

import json
import os
import shlex
import subprocess
import sys
import tempfile

RUN = open(sys.argv[1]).read()
tmp = tempfile.mkdtemp()
bin_ = os.path.join(tmp, "bin")
os.makedirs(bin_)
LOG = os.path.join(tmp, "log")


def stub(name, body):
    path = os.path.join(bin_, name)
    with open(path, "w") as f:
        f.write(f"#!{sys.executable} -I\nimport json, os, sys\nSTUB = {tmp!r}\n"
                "def note(*a):\n    open(os.path.join(STUB, 'log'), 'a').write(json.dumps(a) + '\\n')\n"
                # Whether the runs' lock is held as it's called: by the run
                # that called it, since nothing else takes it here.
                "def locked(what):\n    import subprocess\n"
                "    note('locked', what, subprocess.run(['flock', '-n', os.path.join(STUB, 'lock'), 'true']).returncode != 0)\n"
                + body)
    os.chmod(path, 0o755)
    return path


# GitHub: a repo as it spells it, and what each owner's installation reaches
# (OWNER-repos, one per line, or OWNER-fails to fail).
stub("gh", """a = sys.argv[1:]
note("gh", a, os.environ.get("GH_TOKEN_OWNER"), os.environ.get("GH_TOKEN_SOCKET"))
if a == ["auth", "token"]:
    locked("token")
    print("gho_user")
elif a[:1] == ["api"] and a[1].startswith("repos/"):
    print({"sam/r": "Sam/R"}.get(a[1][6:], a[1][6:]))
elif a[:2] == ["api", "installation/repositories"]:
    owner = os.environ["GH_TOKEN_OWNER"]
    if os.path.exists(os.path.join(STUB, f"{owner}-fails")):
        sys.exit(1)
    print(open(os.path.join(STUB, f"{owner}-repos")).read(), end="")
else:
    sys.exit(f"gh {a}: not a stand-in answer")
""")
# The broker: refuses without its socket, as the real one does, and mints a
# new token each time, so a run's own is told apart.
stub("gh-token", """a = sys.argv[1:]
if not os.environ.get("GH_TOKEN_SOCKET"):
    sys.exit("GH_TOKEN_SOCKET isn't set")
n = sum(1 for _ in open(os.path.join(STUB, "log"))) if os.path.exists(os.path.join(STUB, "log")) else 0
note("gh-token", a)
locked("token")
print(f"ghs_{a[1]}_{n}")
""")
# Renovate: what it was run with, and with whose token; a repo named in
# renovate-fails fails its run.
stub("renovate", """note("renovate", sys.argv[1:], os.environ.get("RENOVATE_TOKEN"), os.environ.get("GITHUB_COM_TOKEN"))
locked("renovate")
fails = os.path.join(STUB, "renovate-fails")
if os.path.exists(fails) and any(open(fails).read().strip() in a for a in sys.argv[1:]):
    sys.exit(1)
""")
# autodiscoverFilter: covers everything but what's named in uncovered, and
# can't be applied to a repo named "broken".
covers = stub("covers", """note("covers", sys.argv[1:])
if sys.argv[1].endswith("/broken"):
    sys.exit(2)
if sys.argv[1] in open(os.path.join(STUB, "uncovered")).read().split():
    sys.exit(10)
""")
appenv = os.path.join(tmp, "workers.env")
with open(appenv, "w") as f:
    f.write("export GH_TOKEN_SOCKET=/run/gh-token/workers.sock\n")


RUNS = os.path.join(tmp, "runs")


def noted():
    """The names runs are noted for."""
    return sorted(os.listdir(RUNS)) if os.path.exists(RUNS) else []


def ended(name):
    """When the runs noted for NAME ended."""
    try:
        return [int(x) for x in open(os.path.join(RUNS, name)).read().split()]
    except FileNotFoundError:
        return []


def run(*args, app=False, owners="", repos=None, fails=(), uncovered=(), renovate_fails=None, keep=False):
    """One run, with no runs noted before it unless keep; (its exit status,
    every call it made, in order)."""
    if not keep and os.path.exists(RUNS):
        for name in os.listdir(RUNS):
            os.remove(os.path.join(RUNS, name))
    for name in os.listdir(tmp):
        if name.endswith(("-repos", "-fails")) or name in ("log", "uncovered", "renovate-fails"):
            os.remove(os.path.join(tmp, name))
    for owner, rs in (repos or {}).items():
        open(os.path.join(tmp, f"{owner}-repos"), "w").write("".join(f"{r}\n" for r in rs))
    for owner in fails:
        open(os.path.join(tmp, f"{owner}-fails"), "w").close()
    open(os.path.join(tmp, "uncovered"), "w").write(" ".join(uncovered))
    if renovate_fails:
        open(os.path.join(tmp, "renovate-fails"), "w").write(renovate_fails)
    env = {"APP_ENV": appenv if app else "", "APP_OWNERS": owners, "COVERS": covers, "RUN_LOCK": os.path.join(tmp, "lock"), "RUNS": RUNS}
    script = "set -euo pipefail\n" + "".join(f"{k}={shlex.quote(v)}\n" for k, v in env.items()) + RUN
    p = subprocess.run(["bash", "-c", script, "renovate-run", *args],
                       env={**{k: v for k, v in os.environ.items() if k not in ("GH_TOKEN_SOCKET", "GH_TOKEN_OWNER")},
                            "PATH": bin_ + ":" + os.environ["PATH"]})
    calls = [json.loads(x) for x in open(LOG)] if os.path.exists(LOG) else []
    # Every token is asked for, and every run made, holding the lock: a token
    # asked for before a wait for it could run out before its run does.
    assert all(c[2] for c in calls if c[0] == "locked"), calls
    return p.returncode, calls


def runs(calls):
    return [(c[1], c[2], c[3]) for c in calls if c[0] == "renovate"]


# As the user: a full run is one Renovate over everything, with gh's login,
# noted as `all`.
code, calls = run()
assert code == 0 and runs(calls) == [([], "gho_user", "gho_user")], calls
assert ["locked", "token", True] in calls and ["locked", "renovate", True] in calls, calls
assert len(ended("all")) == 1 and noted() == ["all"], noted()
# One repo: as GitHub spells it, if the filter covers it, noted by its name
# in lower case.
code, calls = run("sam/r")
assert code == 0 and runs(calls) == [(['--autodiscover-filter=["Sam/R"]'], "gho_user", "gho_user")], calls
assert ["covers", ["Sam/R"]] in calls, calls
assert len(ended("sam_r")) == 1 and noted() == ["sam_r"], noted()
code, calls = run("sam/r", uncovered=["Sam/R"])
assert code == 0 and runs(calls) == [] and ended("sam_r") == [], calls
code, calls = run("sam/broken")
assert code == 1 and runs(calls) == [] and noted() == [], calls
# A run that fails is noted too: it ran. Only the last ten are kept.
code, calls = run("sam/r", renovate_fails="Sam/R")
assert code == 1 and len(ended("sam_r")) == 1, calls
for _ in range(10):
    run("sam/r", keep=True)
assert len(ended("sam_r")) == 10 and ended("sam_r") == sorted(ended("sam_r")), ended("sam_r")
assert not [c for c in calls if c[0] == "gh-token"], calls

# As the App: one repo's run gets its owner's token, never gh's login.
code, calls = run("sam/r", app=True)
assert code == 0 and [r[0] for r in runs(calls)] == [['--autodiscover-filter=["Sam/R"]']], calls
(_, token, com), = runs(calls)
assert token.startswith("ghs_Sam_") and com == token, calls
assert ["locked", "token", True] in calls, calls
assert ["gh-token", ["get", "Sam"]] in calls and ["gh", ["auth", "token"], None, "/run/gh-token/workers.sock"] not in calls, calls
assert not [c for c in calls if c[0] == "gh" and c[1] == ["auth", "token"]], calls
# Every gh call is the App's: its env is the run's.
assert all(c[3] == "/run/gh-token/workers.sock" for c in calls if c[0] == "gh"), calls
code, calls = run("sam/r", app=True, uncovered=["Sam/R"])
assert code == 0 and runs(calls) == [] and not [c for c in calls if c[0] == "gh-token"], calls

# A full run as the App: every repo each owner's installation reaches, a run
# each, with a token of its owner's asked for that run.
code, calls = run(app=True, owners="sam kim", repos={"sam": ["sam/a", "sam/b"], "kim": ["kim/c"]}, uncovered=["sam/b"])
assert code == 0, calls
assert [r[0] for r in runs(calls)] == [['--autodiscover-filter=["sam/a"]'], ['--autodiscover-filter=["kim/c"]']], calls
tokens = [r[1] for r in runs(calls)]
assert tokens[0].startswith("ghs_sam_") and tokens[1].startswith("ghs_kim_"), tokens
listed = [c[2] for c in calls if c[0] == "gh" and c[1][:2] == ["api", "installation/repositories"]]
assert listed == ["sam", "kim"], calls
# Asked again for each run, so none outlives its token.
code, calls = run(app=True, owners="sam", repos={"sam": ["sam/a", "sam/b"]})
assert code == 0 and len({r[1] for r in runs(calls)}) == 2, calls
# One repo's run failing, or one owner's list, doesn't hold up the rest, but
# fails the pass.
code, calls = run(app=True, owners="sam kim", repos={"sam": ["sam/a", "sam/b"], "kim": ["kim/c"]}, renovate_fails="sam/a")
assert code == 1 and [r[0] for r in runs(calls)] == [['--autodiscover-filter=["sam/a"]'], ['--autodiscover-filter=["sam/b"]'],
                                                     ['--autodiscover-filter=["kim/c"]']], calls
assert noted() == ["kim_c", "sam_a", "sam_b"], noted()
code, calls = run(app=True, owners="sam kim", repos={"kim": ["kim/c"]}, fails=["sam"])
assert code == 1 and [r[0] for r in runs(calls)] == [['--autodiscover-filter=["kim/c"]']], calls
code, calls = run(app=True, owners="sam", repos={"sam": ["sam/broken", "sam/a"]})
assert code == 1 and [r[0] for r in runs(calls)] == [['--autodiscover-filter=["sam/a"]']], calls
# Never one run over everything with one token.
assert not [r for r in runs(calls) if r[0] == []], calls

print("renovate-run: ok")
