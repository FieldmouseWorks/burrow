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
    # As Renovate's clone has it: main is the default branch.
    git("update-ref", "refs/remotes/origin/main", "HEAD")
    git("symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main")
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
    if cmd[:3] == ["npm", "config", "get"]:
        return subprocess.CompletedProcess(cmd, 0, "".join(f"{k}={NPMRC.get(k, 'null')}\n" for k in cmd[3:]), "")
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


NPM_TIMES = {}
NPMRC = {}  # npm's own config for the lock file's directory


def get_json(url):
    name = url[len(L.NPM):].replace("%2F", "/")
    return {"time": NPM_TIMES[name]}


# release-ages, Renovate's reading of a config, is checked on its own with
# Renovate (flake.nix); here a stand-in reads days and weeks, and knows
# three presets: "slow" sets 2 weeks, "files" adds ci/slow.json as a config
# file, "inherits" inherits config. Any other preset it can't resolve.
RESOLVED = []


def release_ages(resolver, config):
    assert resolver == ["release-ages"], resolver
    RESOLVED.append(config)
    ages = []
    got = {"inheritConfig": bool(config.get("inheritConfig")),
           "reusesClone": bool(config.get("persistRepoData") or config.get("forkToken"))}
    if "configFileNames" in config:
        got["configFileNames"] = config["configFileNames"]

    def walk(node, under=""):
        if isinstance(node, dict):
            for k, v in node.items():
                if k == "extends":
                    for x in v:
                        if x == "slow":
                            ages.append(14 * 86400)
                        elif x == "files":
                            got.setdefault("configFileNames", ["ci/slow.json"])
                        elif x == "inherits":
                            got["inheritConfig"] = True
                        else:
                            raise RuntimeError(f"can't resolve {x}")
                elif k == "minimumReleaseAge":
                    if v is not None:
                        n, unit = v.split()
                        ages.append(int(n) * {"days": 86400, "weeks": 604800}[unit])
                elif not k.startswith("statusCheck"):
                    walk(v)
        elif isinstance(node, list):
            for v in node:
                walk(v)
    walk(config)
    return {**got, "ages": ages}


# What lock-age makes of release-ages' answer, or its failure.
answer = '{"ages": [86400000, 1000], "inheritConfig": false, "reusesClone": false}'
assert L.release_ages(["sh", "-c", f"cat >/dev/null; echo '{answer}'"], {}) == \
    {"ages": [86400, 1], "inheritConfig": False, "reusesClone": False}
try:
    L.release_ages(["sh", "-c", "echo 'Error: no such preset' >&2; exit 1"], {})
    raise AssertionError("release-ages failed, and lock-age took its answer")
except RuntimeError:
    pass
L.crate_index, L.run, L.get_json, L.release_ages = crate_index, run, get_json, release_ages
POLICY = {"minimumReleaseAge": "3 days"}


def lock_age(policy=POLICY, branch="main"):
    RAN.clear()
    RESOLVED.clear()
    path = os.path.join(tempfile.mkdtemp(), "policy.json")
    write(path, json.dumps(policy))
    return L.main(["lock-age", path, "release-ages", branch])


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
default_branch, L.default_branch = L.default_branch, lambda: "main"
assert lock_age() == 1
L.default_branch = default_branch

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
# A preset it can resolve counts, in the repo's config or the machine's,
# at any scope; one it can't fails the refresh.
for policy, files in ((POLICY, {"renovate.json": '{"extends": ["slow"]}'}),
                      (POLICY, {"renovate.json": '{"packageRules": [{"matchManagers": ["cargo"], "extends": ["slow"]}]}'}),
                      ({**POLICY, "extends": ["slow"]}, {}),
                      ({**POLICY, "cargo": {"extends": ["slow"]}}, {}),
                      ({**POLICY, "globalExtends": ["slow"]}, {})):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), **files})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age(policy) == 0 and versions()["h"] == "1.0.0", (policy, files, versions())
for policy, files in ((POLICY, {"renovate.json5": '{extends: ["github>someone/presets"]}'}),
                      (POLICY, {"renovate.json5": "{minimumReleaseAge: someAge}"}),
                      ({**POLICY, "extends": ["github>someone/presets"]}, {})):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0")), **files})
    write("Cargo.lock", cargo_lock(("b", "2.1.0")))
    assert lock_age(policy) == 1 and RAN == [], (policy, files)
# JSONC, and a config file the machine adds (configFileNames), read first,
# its name whole.
for files, policy in (({"renovate.jsonc": '{/* slow */ "minimumReleaseAge": "2 weeks",}'}, POLICY),
                      ({"renovate.json": "{}", "ci/renovate config.json": '{"minimumReleaseAge": "2 weeks"}'},
                       {**POLICY, "configFileNames": ["ci/renovate config.json"]})):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), **files})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age(policy) == 0 and versions()["h"] == "1.0.0", (files, versions())
# The machine's longest age holds, wherever in its config it's set.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age({**POLICY, "cargo": {"minimumReleaseAge": "1 weeks"}}) == 0 and versions()["h"] == "1.0.0", versions()
# A machine that sets no age at all holds to none, and still to the repo's.
INDEX.update(n={"1.0.0": OLD, "1.1.0": NEW})
for policy in ({"minimumReleaseAge": None}, {}):
    repo({"Cargo.lock": cargo_lock(("n", "1.0.0"))})
    write("Cargo.lock", cargo_lock(("n", "1.1.0")))
    assert lock_age(policy) == 0 and versions() == {"n": "1.1.0"}, (policy, versions())
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), "renovate.json": json.dumps(config)})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age(policy) == 0 and versions()["h"] == "1.0.0", (policy, versions())
# .gitlab/'s config files count, as Renovate looks there on GitHub too.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), ".gitlab/renovate.json5": "{minimumReleaseAge: '2 weeks'}"})
write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
assert lock_age() == 0 and versions()["h"] == "1.0.0", versions()
# A refresh for a branch besides the default (Renovate reads the default
# branch's config for it, not the one checked out) fails, as does one
# where the default branch can't be told.
repo({"Cargo.lock": cargo_lock(("b", "2.0.0"))})
write("Cargo.lock", cargo_lock(("b", "2.1.0")))
assert lock_age(branch="release/1") == 1 and RAN == []
# origin/HEAD is the default only in a fresh clone of the repo itself: a
# machine that keeps clones or works in forks fails every refresh.
for policy in ({**POLICY, "persistRepoData": True}, {**POLICY, "forkToken": "t"}):
    assert lock_age(policy) == 1 and RAN == [], policy
git("symbolic-ref", "--delete", "refs/remotes/origin/HEAD")
assert lock_age() == 1 and RAN == []
# Config the machine inherits from another repo fails every refresh, set
# directly or by a preset.
for policy in ({**POLICY, "inheritConfig": True}, {**POLICY, "extends": ["inherits"]},
               {**POLICY, "globalExtends": ["inherits"]}):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"))})
    write("Cargo.lock", cargo_lock(("b", "2.1.0")))
    assert lock_age(policy) == 1 and RAN == [], policy
# Config files the machine adds are read, from its globalExtends too, but
# not from its ordinary extends (Renovate settles the files before it
# resolves those); the machine's own configFileNames win over
# globalExtends', null included.
for policy, held in (({**POLICY, "extends": ["files"]}, False), ({**POLICY, "globalExtends": ["files"]}, True),
                     ({**POLICY, "globalExtends": ["files"], "configFileNames": []}, False),
                     ({**POLICY, "globalExtends": ["files"], "configFileNames": None}, False)):
    repo({"Cargo.lock": cargo_lock(("b", "2.0.0"), ("h", "1.0.0")), "ci/slow.json": '{"minimumReleaseAge": "2 weeks"}'})
    write("Cargo.lock", cargo_lock(("b", "2.1.0"), ("h", "1.1.0")))
    assert lock_age(policy) == 0 and versions()["h"] == ("1.0.0" if held else "1.1.0"), (policy, versions())

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
    first = [x[0][:2] for x in RAN].count(["npm", "install"]) == 1
    assert os.path.exists(path) != first, "npm must resolve from nothing, then again over that"
    if RESOLVES["code"] if first else RESOLVES.get("second_code"):
        return subprocess.CompletedProcess(cmd, 1, "", "npm error code ETARGET\nnpm error notarget No matching version")
    if first or RESOLVES.get("second"):
        write(path, RESOLVES["lock"] if first else RESOLVES["second"])
    return subprocess.CompletedProcess(cmd, 0, "", "")


def installs():
    """npm install ran twice, the same both times: that command."""
    got = [x for x in RAN if x[0][:2] == ["npm", "install"]]
    assert len(got) == 2 and got[0] == got[1], got
    return got[0]


NPM_DOES = npm_does
NPM_TIMES.update({"left-pad": {"1.0.0": OLD, "1.1.0": OLD, "1.2.0": NEW}, "@s/k": {"2.0.0": OLD, "2.0.1": OLD}})
repo({"web/package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"), ("node_modules/@s/k", "2.0.0"))})
write("web/package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0"), ("node_modules/@s/k", "2.0.1")))
RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.1.0"), ("node_modules/@s/k", "2.0.1"))
assert lock_age() == 0, RAN
cmd, cwd = installs()
before = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=AGE))
got = dt.datetime.strptime(re.search(r"--before=(\S+)", " ".join(cmd)).group(1), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.timezone.utc)
assert cwd == "web" and abs((got - before).total_seconds()) < 60, (cmd, got, before)
assert "left-pad" in open("web/package-lock.json").read() and '"1.1.0"' in open("web/package-lock.json").read()

# npm can't resolve under the cutoff: the refresh fails, it isn't retried without.
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
RESOLVES["code"] = 1
assert lock_age() == 1 and [x[0][:2] for x in RAN].count(["npm", "install"]) == 1, RAN
RESOLVES["code"] = 0

# npm's own config for the directory holds when it's stricter than the age
# (--before would override it): in what npm resolves under, and in what's
# checked after.
for npmrc, days in (({"before": (NOW - dt.timedelta(days=30)).strftime("%a %b %d %Y %H:%M:%S GMT+0000 (UTC)")}, 30),
                    ({"min-release-age": "30"}, 30), ({"before": (NOW - dt.timedelta(days=1)).strftime(
                        "%a %b %d %Y %H:%M:%S GMT+0000 (UTC)")}, 3)):
    NPMRC.update(npmrc)
    repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
    write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
    RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.1.0"))  # 20 days old
    assert lock_age() == (1 if days == 30 else 0), npmrc
    cmd, _ = installs()
    got = dt.datetime.strptime(re.search(r"--before=(\S+)", " ".join(cmd)).group(1), "%Y-%m-%dT%H:%M:%SZ")
    assert abs((got.replace(tzinfo=dt.timezone.utc) - (NOW - dt.timedelta(days=days))).total_seconds()) < 120, (npmrc, cmd)
    NPMRC.clear()
# npm prints before in its local time zone: the same instant, whatever the
# offset; and an npm without min-release-age (npm 10) says undefined.
for offset in (-8, 0, 5.5):
    tz = dt.timezone(dt.timedelta(hours=offset))
    local = (NOW - dt.timedelta(days=30)).astimezone(tz)
    NPMRC.update({"before": local.strftime("%a %b %d %Y %H:%M:%S GMT%z (Somewhere)"), "min-release-age": "undefined"})
    repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
    write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
    RESOLVES["lock"] = npm_lock(("node_modules/left-pad", "1.0.1"))
    NPM_TIMES["left-pad"]["1.0.1"] = (NOW - dt.timedelta(days=31)).strftime("%Y-%m-%dT%H:%M:%SZ")
    assert lock_age() == 0, (offset, NPMRC)
    cmd, _ = installs()
    got = dt.datetime.strptime(re.search(r"--before=(\S+)", " ".join(cmd)).group(1), "%Y-%m-%dT%H:%M:%SZ")
    assert abs((got.replace(tzinfo=dt.timezone.utc) - (NOW - dt.timedelta(days=30))).total_seconds()) < 120, (offset, cmd)
    NPMRC.clear()
NPMRC["before"] = "next tuesday"
repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
assert lock_age() == 1 and not [x for x in RAN if x[0][:2] == ["npm", "install"]]
NPMRC.clear()

# npm's second pass fails, or takes something too new: the refresh fails.
for second in ({"second_code": 1}, {"second": npm_lock(("node_modules/left-pad", "1.2.0"))}):
    repo({"package-lock.json": npm_lock(("node_modules/left-pad", "1.0.0"))})
    write("package-lock.json", npm_lock(("node_modules/left-pad", "1.2.0")))
    RESOLVES.update(lock=npm_lock(("node_modules/left-pad", "1.1.0")), **second)
    assert lock_age() == 1, second
    RESOLVES.pop("second_code", None)
    RESOLVES.pop("second", None)

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

# Arguments: the policy, release-ages and the branch.
for argv in (["lock-age"], ["lock-age", "policy.json"], ["lock-age", "policy.json", "release-ages"]):
    assert L.main(argv) == 2, argv

print("lock-age: ok")
