"""lock-age age=AGE... [preset=PRESET...] [file=NAME...] [opaque=WHAT...]: a lock
file refresh takes only releases at least the longest AGE old, or fails.

Renovate runs it after a lock file refresh (a postUpgradeTask, from the
repo's root, with the refresh in the working tree and HEAD at the base). For
each lock file the refresh changed:

  - Cargo.lock: every crates.io version that isn't in the base's lock file
    and was published within the age is taken back with `cargo update
    --precise` to the newest version old enough, or to the base's. One that
    can't go back because a crate the refresh moved needs it takes that
    crate back to its base version too.
  - package-lock.json: made again from nothing, as the refresh did, with
    npm's `--before` set to the cutoff, and failing if npm can't resolve
    under it (Renovate drops `--before` and resolves again without it).

Then every new version is checked against its registry's publish time. A
version the base already had counts as checked only from the same source.
One still too young, a registry it can't ask (a crate from another
registry, a tarball from elsewhere), a lock file taken all the way back to
the base's (Renovate would then commit its own, unchecked), an npm lock
file without a packages map (npm 6's) or any other lock file changing
(yarn, pnpm) fails the refresh: Renovate shows it as an artifact failure
and doesn't merge it. A git dependency's commit is taken as it is, as Nix
inputs are, in Cargo.lock and package-lock.json alike (burrow#82).

The machine's Renovate config gives every release age it sets (AGEs,
Renovate's durations without their spaces: 3days), the presets it extends
and the config files it adds; opaque= names a setting that brings in config
lock-age can't see (globalExtends, inheritConfig), and fails every refresh.
The age is the longest of those and of every minimumReleaseAge the repo's
own config sets. A repo extending a preset the machine doesn't fails the
refresh too, as that preset could set an age lock-age can't see.
"""

import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
import tomllib
import urllib.parse
import urllib.request

import json5

UA = "burrow lock-age (https://github.com/FieldmouseWorks/burrow)"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
NPM = "https://registry.npmjs.org/"
ROUNDS = 6  # passes over a Cargo.lock before what's left is a failure
TRIES = 5  # older versions asked about per crate per pass
# Where Renovate looks for a repo's own config on GitHub, in its order,
# after any configFileNames the machine adds (file=).
CONFIGS = ("renovate.json", "renovate.jsonc", "renovate.json5", ".github/renovate.json",
           ".github/renovate.jsonc", ".github/renovate.json5", ".renovaterc", ".renovaterc.json",
           ".renovaterc.jsonc", ".renovaterc.json5", "package.json")
SECONDS = {"s": 1, "sec": 1, "second": 1, "m": 60, "min": 60, "minute": 60, "h": 3600, "hr": 3600,
           "hour": 3600, "d": 86400, "day": 86400, "w": 604800, "wk": 604800, "week": 604800,
           "mo": 2592000, "month": 2592000, "y": 31536000, "yr": 31536000, "year": 31536000}


def log(*a):
    print("lock-age:", *a, file=sys.stderr, flush=True)


def run(cmd, cwd):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


_last = [0.0]


def get_json(url):
    # crates.io asks for at most one request a second, with a user agent.
    if url.startswith("https://crates.io/"):
        wait = _last[0] + 1 - time.monotonic()
        if wait > 0:
            time.sleep(wait)
        _last[0] = time.monotonic()
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def get_lines(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return [json.loads(x) for x in r.read().decode().splitlines() if x.strip()]


def when(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00"))


def index_path(name):
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


_index = {}


def crate_index(name):
    """The crate's versions from the sparse index, by version."""
    if name not in _index:
        _index[name] = {x["vers"]: x for x in get_lines(f"https://index.crates.io/{index_path(name)}")}
    return _index[name]


def crate_published(name, version):
    """The index's publish time; crates.io's API for a version from before
    the index had them."""
    entry = crate_index(name).get(version) or {}
    if entry.get("pubtime"):
        return when(entry["pubtime"])
    got = get_json(f"https://crates.io/api/v1/crates/{urllib.parse.quote(name)}/{urllib.parse.quote(version)}")
    return when(got["version"]["created_at"])


def crate_versions(name):
    """Every version that isn't yanked."""
    return [v for v, x in crate_index(name).items() if not x.get("yanked")]


_npm_times = {}


def npm_published(name, version):
    if name not in _npm_times:
        _npm_times[name] = get_json(NPM + urllib.parse.quote(name, safe="@"))["time"]
    t = _npm_times[name].get(version)
    if t is None:
        raise LookupError(f"the registry has no publish time for {name}@{version}")
    return when(t)


def semver_key(v):
    """Sorts versions oldest first; a pre-release sorts before its release."""
    core, _, pre = v.partition("+")[0].partition("-")
    nums = [int(x) if x.isdigit() else 0 for x in core.split(".")]
    return (nums, 0 if pre else 1, pre)


def base_text(path):
    p = subprocess.run(["git", "show", f"HEAD:{path}"], capture_output=True, text=True)
    return p.stdout if p.returncode == 0 else None


def duration(text):
    """Renovate's "3 days", "1 week 2 days" or "12h", in seconds."""
    terms = re.findall(r"\s*(\d+)\s*([a-z]+)", text.lower())
    if not terms or "".join(n + u for n, u in terms) != re.sub(r"\s", "", text.lower()):
        raise ValueError(f"can't read the duration {text!r}")
    total = 0
    for n, unit in terms:
        unit = unit if unit in SECONDS else unit[:-1] if unit.endswith("s") else unit
        if unit not in SECONDS:
            raise ValueError(f"can't read the duration {text!r}")
        total += int(n) * SECONDS[unit]
    return total


def walk(node, key, under=None):
    """Every value set for key anywhere in a config: the top level, a
    manager's or update type's block, a package rule. statusCheckNames and
    statusCheckWhen use minimumReleaseAge for a check's name and when it's
    set, not for an age."""
    if isinstance(node, dict):
        for k, value in node.items():
            if (under or "").startswith("statusCheck"):
                continue
            if k == key:
                yield value
            else:
                yield from walk(value, key, k)
    elif isinstance(node, list):
        for value in node:
            yield from walk(value, key, under)


def repo_age(presets, files):
    """The longest minimumReleaseAge in the base's own Renovate config, in
    seconds. Renovate reads only the first config file it finds, and
    package.json only for its "renovate" key. A preset other than the
    machine's own, extended anywhere in it, could set an age lock-age
    can't see, so it fails the refresh."""
    for name in (*files, *CONFIGS):
        text = base_text(name)
        if text is None:
            continue
        config = json5.loads(text)
        if name == "package.json":
            if "renovate" not in config:
                continue
            config = config["renovate"]
        if not isinstance(config, dict):
            raise ValueError(f"{name}: not a Renovate config")
        unknown = []
        for extends in walk(config, "extends"):
            unknown += [x for x in (extends if isinstance(extends, list) else [extends]) if x not in presets]
        if unknown:
            raise ValueError(f"{name}: extends {', '.join(map(str, unknown))}, whose minimumReleaseAge lock-age can't see")
        return max((duration(value) for value in walk(config, "minimumReleaseAge") if value is not None), default=0)
    return 0


def args(argv):
    """age=, preset=, file= and opaque= arguments, by kind."""
    got = {"age": [], "preset": [], "file": [], "opaque": []}
    for arg in argv[1:]:
        kind, _, value = arg.partition("=")
        if kind not in got or not value:
            raise ValueError(arg)
        got[kind].append(value)
    if not got["age"]:
        raise ValueError("no age=")
    return got


def changed():
    """Every file the refresh changed, added or left untracked. NUL-separated,
    so no path is quoted or split."""
    paths = set()
    for cmd in (["git", "diff", "-z", "--name-only", "HEAD"], ["git", "ls-files", "-z", "--others", "--exclude-standard"]):
        p = run(cmd, ".")
        if p.returncode != 0:
            raise RuntimeError(f"{' '.join(cmd)}: {p.stderr.strip()}")
        paths.update(x for x in p.stdout.split("\0") if x)
    return sorted(paths)


def same_as_base(path, base):
    with open(path) as f:
        return f.read() == base


def cargo_packages(text):
    return tomllib.loads(text).get("package", []) if text else []


def ident(p):
    return p["name"], p["version"], p.get("source")


def cargo_new(path, base):
    """The packages in the lock file that the base's doesn't have from the
    same source (a path crate has none, and is the repo's own)."""
    had = {ident(p) for p in cargo_packages(base)}
    with open(path) as f:
        now = cargo_packages(f.read())
    return [p for p in now if ident(p) not in had and "source" in p], now


def cargo_young(path, base, cutoff, bad):
    young = []
    new, _ = cargo_new(path, base)
    for p in new:
        if p["source"].startswith("git+"):
            continue  # a commit, as a Nix input is
        if p["source"] != CRATES_IO:
            bad.append(f"{path}: {p['name']} {p['version']} is from {p['source']}, whose publish times aren't known")
            continue
        if crate_published(p["name"], p["version"]) > cutoff:
            young.append(p)
    return young


def precise(d, name, version, to):
    p = run(["cargo", "update", "--quiet", "-p", f"{name}@{version}", "--precise", to], d)
    if p.returncode != 0:
        log(f"{d}: {name} {version} -> {to}: {p.stderr.strip().splitlines()[-1:] or ['failed']}")
    return p.returncode == 0


def cargo(path, cutoff, bad):
    d = os.path.dirname(path) or "."
    base = base_text(path)
    # The base's crates.io versions, the only ones to go back to.
    base_versions = {}
    for p in cargo_packages(base):
        if p.get("source") == CRATES_IO:
            base_versions.setdefault(p["name"], []).append(p["version"])
    for _ in range(ROUNDS):
        young = cargo_young(path, base, cutoff, [])
        if not young:
            break
        for p in young:
            name, version = p["name"], p["version"]
            older = sorted((v for v in crate_versions(name) if semver_key(v) < semver_key(version)),
                           key=semver_key, reverse=True)
            done = False
            for v in older[:TRIES]:
                if crate_published(name, v) <= cutoff and precise(d, name, version, v):
                    log(f"{path}: {name} {version} is too new, took {v}")
                    done = True
                    break
            for v in [] if done else sorted(base_versions.get(name, []), key=semver_key, reverse=True):
                if precise(d, name, version, v):
                    log(f"{path}: {name} {version} is too new, kept the base's {v}")
                    done = True
                    break
            if done:
                continue
            # Something the refresh moved needs this version: take those
            # back to the base's version, and this one goes back next pass.
            _, now = cargo_new(path, base)
            with open(path) as f:
                pkgs = cargo_packages(f.read())
            moved = {ident(q) for q in now}
            for q in pkgs:
                needs = {x.split(" ")[0] for x in q.get("dependencies", [])}
                if name in needs and ident(q) in moved and q.get("source") == CRATES_IO and q["name"] in base_versions:
                    to = max(base_versions[q["name"]], key=semver_key)
                    if precise(d, q["name"], q["version"], to):
                        log(f"{path}: {q['name']} {q['version']} needs {name} {version}, kept the base's {to}")
    for p in cargo_young(path, base, cutoff, bad):
        bad.append(f"{path}: {p['name']} {p['version']} was published within the age, and couldn't be taken back")
    if same_as_base(path, base):
        bad.append(f"{path}: nothing in the refresh was old enough, so it went back to the base's, and Renovate "
                   "would commit its own instead")


def npm_entries(lock):
    """(name, version, resolved) of every installed package in a lock file
    with a packages map (npm 7 on)."""
    got = []
    for key, p in lock["packages"].items():
        # The root, workspace members and links are the repo's own, and a
        # bundled one comes inside its parent's tarball.
        if "node_modules/" not in key or p.get("link") or p.get("inBundle") or "version" not in p:
            continue
        got.append((p.get("name") or key.rpartition("node_modules/")[2], p["version"], p.get("resolved")))
    return got


def npm(path, cutoff, bad):
    d = os.path.dirname(path) or "."
    base = base_text(path)
    os.remove(path)
    p = run(["npm", "install", "--package-lock-only", "--ignore-scripts", "--no-audit", "--no-fund",
             f"--before={cutoff.strftime('%Y-%m-%dT%H:%M:%SZ')}"], d)
    if p.returncode != 0:
        bad.append(f"{path}: npm couldn't resolve with only releases from before {cutoff:%Y-%m-%d %H:%M}Z: "
                   + (p.stderr.strip().splitlines() or ["no output"])[-1])
        return
    with open(path) as f:
        lock = json.load(f)
    if lock.get("lockfileVersion", 0) < 2 or not isinstance(lock.get("packages"), dict):
        bad.append(f"{path}: lock file version {lock.get('lockfileVersion')} has no packages map to check")
        return
    if same_as_base(path, base):
        bad.append(f"{path}: nothing in the refresh was old enough, so it came out as the base's, and Renovate "
                   "would commit its own instead")
        return
    try:
        had = set(npm_entries(json.loads(base))) if base else set()
    except (ValueError, KeyError, AttributeError):
        had = set()  # nothing the base had counts as checked: all of it is
    for name, version, resolved in npm_entries(lock):
        if (name, version, resolved) in had or re.fullmatch(r"git\+\S+#[0-9a-f]{40}", resolved or ""):
            continue  # one the base had, or a pinned git commit, as Cargo's are
        if not (resolved or "").startswith(NPM):
            bad.append(f"{path}: {name} {version} comes from {resolved}, whose publish time isn't known")
        elif npm_published(name, version) > cutoff:
            bad.append(f"{path}: {name} {version} was published within the age")


def main(argv):
    try:
        a = args(argv)
        ages = [duration(x) for x in a["age"]]
    except ValueError as e:
        print(__doc__.split("\n\n")[0], f"({e})", file=sys.stderr)
        return 2
    if a["opaque"]:
        log(f"the machine's Renovate config uses {', '.join(a['opaque'])}, which could set a release age "
            "lock-age can't see")
        return 1
    bad = []
    try:
        age = max(*ages, repo_age(set(a["preset"]), a["file"]))
        paths = changed()
    except Exception as e:
        log(f"{type(e).__name__}: {e}")
        return 1
    cutoff = dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=age)
    for path in paths:
        name = os.path.basename(path)
        try:
            if name == "Cargo.lock":
                cargo(path, cutoff, bad)
            elif name == "package-lock.json":
                npm(path, cutoff, bad)
            elif name in ("yarn.lock", "pnpm-lock.yaml", "bun.lock", "bun.lockb", "npm-shrinkwrap.json"):
                bad.append(f"{path}: lock-age can't check this kind of lock file")
        except Exception as e:  # a registry that didn't answer proves nothing
            bad.append(f"{path}: {type(e).__name__}: {e}")
    for line in bad:
        log(line)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
