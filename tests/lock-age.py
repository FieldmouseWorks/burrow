"""lock-age with stand-ins for cargo, npm and the registries: a refresh's
too-new releases are taken back or fail it, and nothing else does.

    python3 -I tests/lock-age.py modules/updates/lock-age.py
"""

import datetime as dt
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location("lock_age", sys.argv[1])
L = importlib.util.module_from_spec(spec)
spec.loader.exec_module(L)

NOW = dt.datetime.now(dt.timezone.utc)
AGE = 3 * 86400
OLD = (NOW - dt.timedelta(days=10)).strftime("%Y-%m-%dT%H:%M:%SZ")
NEW = (NOW - dt.timedelta(hours=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
CRATES = "registry+https://github.com/rust-lang/crates.io-index"


def git(*a):
    subprocess.run(["git", *a], check=True, capture_output=True)


def repo(files):
    d = tempfile.mkdtemp()
    os.chdir(d)
    git("init", "-q")
    for path, text in files.items():
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
    git("add", "-A")
    git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base")
    return d


def write(path, text):
    with open(path, "w") as f:
        f.write(text)


def cargo_lock(*pkgs):
    out = "version = 4\n"
    for name, version, *rest in pkgs:
        source = rest[0] if rest else CRATES
        deps = rest[1] if len(rest) > 1 else []
        out += f'\n[[package]]\nname = "{name}"\nversion = "{version}"\n'
        if source:
            out += f'source = "{source}"\n'
        if deps:
            out += "dependencies = [\n" + "".join(f' "{x}",\n' for x in deps) + "]\n"
    return out


# The index: each crate's versions and when they came out.
INDEX = {}
RAN = []
# cargo update --precise: refused while a predicate on the lock file holds.
REFUSE = []


def crate_index(name):
    return {v: {"vers": v, "pubtime": t} for v, t in INDEX[name].items()}


def run(cmd, cwd):
    if cmd[0] != "git":
        RAN.append((cmd, cwd))
    if cmd[:2] == ["cargo", "update"]:
        name, version = cmd[cmd.index("-p") + 1].split("@")
        to = cmd[cmd.index("--precise") + 1]
        path = os.path.join(cwd, "Cargo.lock")
        text = open(path).read()
        if any(r(text, name, to) for r in REFUSE):
            return subprocess.CompletedProcess(cmd, 101, "", f"error: failed to select a version for {name}")
        write(path, text.replace(f'name = "{name}"\nversion = "{version}"', f'name = "{name}"\nversion = "{to}"'))
        return subprocess.CompletedProcess(cmd, 0, "", "")
    if cmd[:2] == ["npm", "install"]:
        return NPM_DOES(cmd, cwd)
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


NPM_TIMES = {}


def get_json(url):
    name = url[len(L.NPM):].replace("%2F", "/")
    return {"time": NPM_TIMES[name]}


L.crate_index, L.run, L.get_json = crate_index, run, get_json


def lock_age():
    RAN.clear()
    return L.main(["lock-age", str(AGE)])


def versions(path="Cargo.lock"):
    return {p["name"]: p["version"] for p in L.cargo_packages(open(path).read())}


# Nothing changed: nothing to do.
repo({"Cargo.lock": cargo_lock(("a", "1.0.0"))})
assert lock_age() == 0 and RAN == [], RAN

# A refresh that took a too-new crate takes the newest one old enough
# instead; one old enough stays, as do a git dependency's commit and a path
# crate of the workspace's own.
INDEX.update(a={"1.0.0": OLD, "1.0.1": OLD, "1.0.2": NEW}, b={"2.0.0": OLD, "2.1.0": OLD})
repo({"Cargo.lock": cargo_lock(("a", "1.0.0"), ("b", "2.0.0"), ("me", "0.1.0", None))})
write("Cargo.lock", cargo_lock(("a", "1.0.2"), ("b", "2.1.0"), ("g", "0.3.0", "git+https://example.org/g#abc"),
                               ("me", "0.1.0", None)))
assert lock_age() == 0
assert versions() == {"a": "1.0.1", "b": "2.1.0", "g": "0.3.0", "me": "0.1.0"}, versions()

# In a member's directory: cargo runs there.
INDEX.update(c={"0.4.0": OLD, "0.4.1": NEW})
repo({"server/Cargo.lock": cargo_lock(("c", "0.4.0"))})
write("server/Cargo.lock", cargo_lock(("c", "0.4.1")))
assert lock_age() == 0 and versions("server/Cargo.lock") == {"c": "0.4.0"}
assert [cwd for cmd, cwd in RAN if cmd[0] == "cargo"] == ["server"], RAN

# None older than the new one is old enough, but the base's is: back to it.
INDEX.update(d={"1.0.0": NEW, "1.1.0": NEW})
INDEX["d"]["0.9.0"] = OLD  # the base's, older than every other
repo({"Cargo.lock": cargo_lock(("d", "0.9.0"))})
INDEX["d"]["0.9.0"] = NEW  # (as a base can hold a too-new one, merged on purpose)
write("Cargo.lock", cargo_lock(("d", "1.1.0")))
assert lock_age() == 0 and versions() == {"d": "0.9.0"}, versions()

# A crate the refresh moved needs the too-new one: that crate goes back to
# the base's version too, and then the too-new one can.
INDEX.update(e={"1.0.0": OLD, "1.0.1": NEW}, q={"1.0.0": OLD, "1.1.0": OLD})
repo({"Cargo.lock": cargo_lock(("e", "1.0.0"), ("q", "1.0.0", CRATES, ["e"]))})
write("Cargo.lock", cargo_lock(("e", "1.0.1"), ("q", "1.1.0", CRATES, ["e"])))
REFUSE.append(lambda text, name, to: name == "e" and 'name = "q"\nversion = "1.1.0"' in text)
assert lock_age() == 0 and versions() == {"e": "1.0.0", "q": "1.0.0"}, versions()

# One that can't go back at all fails the refresh.
REFUSE.append(lambda text, name, to: name == "e")
repo({"Cargo.lock": cargo_lock(("e", "1.0.0"))})
write("Cargo.lock", cargo_lock(("e", "1.0.1")))
assert lock_age() == 1 and versions() == {"e": "1.0.1"}
REFUSE.clear()

# A crate from a registry whose publish times it can't read fails it too.
repo({"Cargo.lock": cargo_lock(("f", "1.0.0", "registry+https://example.org/index"))})
write("Cargo.lock", cargo_lock(("f", "1.1.0", "registry+https://example.org/index")))
assert lock_age() == 1

# So does a registry that doesn't answer.
repo({"Cargo.lock": cargo_lock(("unknown", "1.0.0"))})
write("Cargo.lock", cargo_lock(("unknown", "1.1.0")))
assert lock_age() == 1


# npm: the lock file is made again from nothing with --before at the cutoff.
def npm_lock(*pkgs):
    packages = {"": {"name": "app", "version": "1.0.0"}, "packages/w": {"name": "w", "version": "0.0.1"},
                "node_modules/w": {"resolved": "packages/w", "link": True}}
    for key, version, *rest in pkgs:
        packages[key] = {"version": version, "resolved": rest[0] if rest else f"{L.NPM}x/-/x-{version}.tgz"}
    return json.dumps({"lockfileVersion": 3, "packages": packages}, indent=2)


RESOLVES = {"lock": None, "code": 0}


def npm_does(cmd, cwd):
    path = os.path.join(cwd, "package-lock.json")
    assert not os.path.exists(path), "npm must resolve from nothing"
    if RESOLVES["code"]:
        return subprocess.CompletedProcess(cmd, 1, "", "npm error code ETARGET\nnpm error notarget No matching version")
    write(path, RESOLVES["lock"])
    return subprocess.CompletedProcess(cmd, 0, "", "")


NPM_DOES = npm_does
NPM_TIMES.update({"left-pad": {"1.0.0": OLD, "1.1.0": OLD, "1.2.0": NEW}, "@s/k": {"2.0.0": OLD, "2.0.1": OLD}})
repo({"web/package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"), ("node_modules/@s/k", "2.0.0"))})
write("web/package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0"), ("node_modules/@s/k", "2.0.1")))
RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.1.0"), ("node_modules/@s/k", "2.0.1"))
assert lock_age() == 0, RAN
(cmd, cwd), = RAN
before = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=AGE))
got = dt.datetime.strptime(re.search(r"--before=(\S+)", " ".join(cmd)).group(1), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
assert cwd == "web" and abs((got - before).total_seconds()) < 60, (cmd, got, before)
assert "left-pad" in open("web/package-lock.json").read() and '"1.1.0"' in open("web/package-lock.json").read()

# npm can't resolve under the cutoff: the refresh fails, it isn't retried without.
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
RESOLVES["code"] = 1
assert lock_age() == 1 and len(RAN) == 1, RAN
RESOLVES["code"] = 0

# npm resolved to something too new anyway, or from somewhere else: it fails.
for bad in (("node_modules/left-pad", "1.2.0"), ("node_modules/left-pad", "1.1.0", "https://example.org/lp.tgz")):
    repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
    write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
    RESOLVES["lock"] = npm_lock(bad)
    assert lock_age() == 1, bad

# A lock file it can't check fails the refresh.
repo({"yarn.lock": "# old\n"})
write("yarn.lock", "# new\n")
assert lock_age() == 1

# Arguments: the age in seconds, nothing else.
assert L.main(["lock-age"]) == 2 and L.main(["lock-age", "3 days"]) == 2

print("lock-age: ok")
