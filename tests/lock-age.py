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
OLD = (NOW - dt.timedelta(days=20)).strftime("%Y-%m-%dT%H:%M:%SZ")
NEW = (NOW - dt.timedelta(hours=5)).strftime("%Y-%m-%dT%H:%M:%SZ")
MID = (NOW - dt.timedelta(days=5)).strftime("%Y-%m-%dT%H:%M:%SZ")  # past the 3 days, not 14
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


def lock_age(*more):
    RAN.clear()
    return L.main(["lock-age", f"age={AGE}s", *more])


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
repo({"server/Cargo.lock": cargo_lock(("b", "2.0.0"), ("c", "0.4.0"))})
write("server/Cargo.lock", cargo_lock(("b", "2.1.0"), ("c", "0.4.1")))
assert lock_age() == 0 and versions("server/Cargo.lock") == {"b": "2.1.0", "c": "0.4.0"}
assert [cwd for cmd, cwd in RAN if cmd[0] == "cargo"] == ["server"], RAN

# None older than the new one is old enough, but the base's is: back to it.
INDEX.update(d={"1.0.0": NEW, "1.1.0": NEW})
INDEX["d"]["0.9.0"] = OLD  # the base's, older than every other
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("d", "0.9.0"))})
INDEX["d"]["0.9.0"] = NEW  # (as a base can hold a too-new one, merged on purpose)
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("d", "1.1.0")))
assert lock_age() == 0 and versions() == {"b": "2.1.0", "d": "0.9.0"}, versions()

# A crate the refresh moved needs the too-new one: that crate goes back to
# the base's version too, and then the too-new one can.
INDEX.update(e={"1.0.0": OLD, "1.0.1": NEW}, q={"1.0.0": OLD, "1.1.0": OLD})
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("e", "1.0.0"), ("q", "1.0.0", CRATES, ["e"]))})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("e", "1.0.1"), ("q", "1.1.0", CRATES, ["e"])))
REFUSE.append(lambda text, name, to: name == "e" and 'name = "q"\nversion = "1.1.0"' in text)
assert lock_age() == 0 and versions() == {"b": "2.1.0", "e": "1.0.0", "q": "1.0.0"}, versions()

# Taken all the way back to the base's, the lock file would look unchanged,
# and Renovate would commit its own refresh instead: that fails it.
repo({"Cargo.lock": cargo_lock(("c", "0.4.0"))})
write("Cargo.lock", cargo_lock(("c", "0.4.1")))
assert lock_age() == 1 and versions() == {"c": "0.4.0"}, versions()

# Even when another lock file did change.
repo({"Cargo.lock": cargo_lock(("c", "0.4.0")), "x/Cargo.lock": cargo_lock(("b", "2.0.0"))})
write("Cargo.lock", cargo_lock(("c", "0.4.1")))
write("x/Cargo.lock", cargo_lock(("b", "2.1.0")))
assert lock_age() == 1

# Paths come through whole: a space, a tab, a quote, non-ASCII.
for d in ("my dir", "caf\u00e9", 'a\t"b'):
    repo({f"{d}/Cargo.lock": cargo_lock(("b", "2.0.0"), ("c", "0.4.0"))})
    write(f"{d}/Cargo.lock", cargo_lock(("b", "2.1.0"), ("c", "0.4.1")))
    assert lock_age() == 0 and versions(f"{d}/Cargo.lock") == {"b": "2.1.0", "c": "0.4.0"}, d

# A git that can't say what changed (no base commit here) fails it.
os.chdir(tempfile.mkdtemp())
git("init", "-q")
write("Cargo.lock", cargo_lock(("c", "0.4.1")))
assert lock_age() == 1

# A version the base had from another source isn't taken as checked: here
# the base's f 1.0.0 was a git dependency, the refresh's is from crates.io.
INDEX.update(f={"0.9.0": OLD, "1.0.0": NEW})
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("f", "1.0.0", "git+https://example.org/f#abc"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("f", "1.0.0")))
assert lock_age() == 0 and versions() == {"b": "2.1.0", "f": "0.9.0"}, versions()

# The repo's own config asks for longer (anywhere in it): that age holds.
INDEX.update(h={"1.0.0": OLD, "1.1.0": MID})
config = {"packageRules": [{"matchManagers": ["cargo"], "minimumReleaseAge": "2 weeks"}],
          "vulnerabilityAlerts": {"minimumReleaseAge": None}}
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), ".github/renovate.json": json.dumps(config)})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions() == {"b": "2.1.0", "h": "1.0.0"}, versions()
# Without it, 5 days is old enough.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions() == {"b": "2.1.0", "h": "1.1.0"}, versions()
# What isn't an age doesn't count: a status check's name, a comment, a
# description, package.json without a "renovate" key.
for name, text in (("renovate.json5", "// minimumReleaseAge: 2 weeks, one day\n{statusCheckNames: "
                    "{minimumReleaseAge: 'custom/stability-days'}, description: 'minimumReleaseAge'}"),
                   ("package.json", json.dumps({"name": "x", "minimumReleaseAge": "2 weeks"}))):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), name: text})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age() == 0 and versions()["h"] == "1.1.0", name
# package.json's "renovate" key is a config.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")),
      "package.json": json.dumps({"renovate": {"cargo": {"minimumReleaseAge": "14 days"}}})})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions()["h"] == "1.0.0", versions()
# Renovate reads only the first config file it finds.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), "renovate.json": "{}",
      ".github/renovate.json": json.dumps(config)})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions()["h"] == "1.1.0", versions()
# One it can't read, or a preset other than the machine's, fails the refresh.
for text in ("{minimumReleaseAge: someAge}", '{"minimumReleaseAge": 5}', '{"minimumReleaseAge": "soon"}',
             '{"extends": ["config:recommended", "github>someone/presets"]}'):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0")), "renovate.json5": text})
    write("Cargo.lock", cargo_lock(("b", "2.1.0")))
    assert lock_age("preset=config:recommended") == 1, text
repo({"Cargo.lock": cargo_lock(("b", "2.0.0")), "renovate.json": '{"extends": ["config:recommended"]}'})
write("Cargo.lock", cargo_lock(("b", "2.1.0")))
assert lock_age("preset=config:recommended") == 0
# A preset extended inside a rule counts too.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0")),
      "renovate.json": '{"packageRules": [{"matchManagers": ["cargo"], "extends": ["github>someone/slow"]}]}'})
write("Cargo.lock", cargo_lock(("b", "2.1.0")))
assert lock_age("preset=config:recommended") == 1
# statusCheckWhen's minimumReleaseAge says when the check is set, not an age.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")),
      "renovate.json": '{"statusCheckWhen": {"minimumReleaseAge": "always"}}'})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions()["h"] == "1.1.0", versions()
# JSONC, and a config file the machine adds (file=), read first.
for files, more in (({"renovate.jsonc": '{/* slow */ "minimumReleaseAge": "2 weeks",}'}, ()),
                    ({"renovate.json": "{}", "ci/renovate.json": '{"minimumReleaseAge": "2 weeks"}'}, ("file=ci/renovate.json",))):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), **files})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age(*more) == 0 and versions()["h"] == "1.0.0", (files, versions())
# The machine's longest age holds.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age("age=1week") == 0 and versions()["h"] == "1.0.0", versions()
# Config the machine brings in that lock-age can't see fails every refresh.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0")))
assert lock_age("opaque=globalExtends") == 1 and RAN == []
assert [L.duration(x) for x in ("3 days", "1 week 2 days", "12h", "1 month")] == [259200, 777600, 43200, 2592000]
for x in ("3", "soon", "3 fortnights"):
    try:
        L.duration(x)
        raise AssertionError(x)
    except ValueError:
        pass

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

# Resolved under the cutoff to just what the base had: fails, as for Cargo.
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.0.0"))
assert lock_age() == 1

# A version the base had from another tarball isn't taken as checked.
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.2.0", "https://example.org/lp.tgz"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.2.0"))
assert lock_age() == 1

# A git dependency's new commit is taken as it is, as Cargo's are; its
# registry dependencies are still checked.
GIT = "git+ssh://git@github.com/o/g.git#"
repo({"package-lock.json": npm_lock(("node_modules/g", "1.0.0", GIT + "a" * 40), ("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/g", "1.0.0", GIT + "b" * 40), ("node_modules/left-pad", "1.2.0")))
RESOLVES["lock"] = npm_lock(("node_modules/g", "1.0.0", GIT + "b" * 40), ("node_modules/left-pad", "1.1.0"))
assert lock_age() == 0
RESOLVES["lock"] = npm_lock(("node_modules/g", "1.0.0", GIT + "b" * 40), ("node_modules/left-pad", "1.2.0"))
assert lock_age() == 1

# npm 6's lock files have no packages map to check: they fail it.
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
RESOLVES["lock"] = json.dumps({"lockfileVersion": 1, "dependencies": {"left-pad": {"version": "1.1.0"}}})
assert lock_age() == 1

# A lock file it can't check fails the refresh.
repo({"yarn.lock": "# old\n"})
write("yarn.lock", "# new\n")
assert lock_age() == 1

# Arguments: at least one age=, and only the kinds lock-age knows.
for argv in (["lock-age"], ["lock-age", "259200"], ["lock-age", "preset=x"], ["lock-age", "age=soon"],
             ["lock-age", "age=3days", "colour=red"]):
    assert L.main(argv) == 2, argv

print("lock-age: ok")
