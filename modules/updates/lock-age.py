"""lock-age POLICY RELEASE-AGES...: a lock file refresh takes only releases at
least the longest release age its Renovate config sets old, or fails.

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

POLICY is the machine's own Renovate config (JSON). The age is the longest
minimumReleaseAge set anywhere in it, in its globalExtends, or in the
repo's own config (in the config files the resolved policy adds, then
Renovate's own), with every config migrated and every preset each extends
resolved, as RELEASE-AGES (updates/release-ages.mjs, Renovate's own code)
reads them; no age set anywhere is none. A preset it can't resolve, an
age it can't read, or inheritConfig (config from another repo it can't
see) fails the refresh. For npm, a cutoff npm's own config sets for the
lock file's directory (before, min-release-age) holds too when it's
earlier.
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
# Where Renovate looks for a repo's own config, in its order
# (config/app-strings.ts; it doesn't filter by platform there), after any
# configFileNames the machine adds.
CONFIGS = ("renovate.json", "renovate.jsonc", "renovate.json5", ".github/renovate.json",
           ".github/renovate.jsonc", ".github/renovate.json5", ".gitlab/renovate.json",
           ".gitlab/renovate.jsonc", ".gitlab/renovate.json5", ".renovaterc", ".renovaterc.json",
           ".renovaterc.jsonc", ".renovaterc.json5", "package.json")


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


def repo_config(files):
    """The base's own Renovate config, or None. Renovate reads only the
    first config file it finds, and package.json only for its "renovate"
    key."""
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
        return config
    return None


def release_ages(resolver, config):
    """What the config sets that decides the age, presets resolved:
    ages (in seconds), configFileNames (only when set, null included),
    inheritConfig and baseBranchPatterns."""
    p = subprocess.run(resolver, input=json.dumps(config), capture_output=True, text=True)
    if p.returncode != 0:
        why = [x for x in p.stderr.splitlines() if "Error" in x or "validationError" in x] or ["failed"]
        raise RuntimeError(f"can't tell the release ages: {why[-1].strip()}")
    got = json.loads(p.stdout)
    return {**got, "ages": [ms / 1000 for ms in got["ages"]]}


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


def npm_cutoff(d, cutoff):
    """The earliest of the cutoff and any npm's own config sets for the
    directory (before, min-release-age), which --before would override."""
    p = run(["npm", "config", "get", "before", "min-release-age"], d)
    if p.returncode != 0:
        raise RuntimeError(f"npm config get: {p.stderr.strip()}")
    # Unset is null; an npm without the option (npm 10's min-release-age)
    # says undefined.
    got = {k: v for k, v in (x.split("=", 1) for x in p.stdout.splitlines() if "=" in x)
           if v not in ("null", "undefined", "")}
    cutoffs = [cutoff]
    if "before" in got:
        # A JavaScript date, in the local time zone: Fri Mar 01 2024 12:30:00 GMT-0800 (...)
        m = re.match(r"\w{3} (\w{3} \d{2} \d{4} \d{2}:\d{2}:\d{2}) GMT([+-]\d{4})", got["before"])
        if not m:
            raise ValueError(f"can't read npm's before {got['before']!r}")
        cutoffs.append(dt.datetime.strptime(" ".join(m.groups()), "%b %d %Y %H:%M:%S %z"))
    if "min-release-age" in got:
        cutoffs.append(dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=float(got["min-release-age"])))
    return min(cutoffs).astimezone(dt.timezone.utc)


def npm(path, cutoff, bad):
    d = os.path.dirname(path) or "."
    base = base_text(path)
    cutoff = npm_cutoff(d, cutoff)
    os.remove(path)
    # Twice: a lock file made from nothing can come out inconsistent with
    # package.json until npm runs again over it (Renovate's npmInstallTwice).
    for _ in range(2):
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
    if len(argv) < 3:
        print(__doc__.split("\n\n")[0], file=sys.stderr)
        return 2
    resolver = argv[2:]
    bad = []
    try:
        with open(argv[1]) as f:
            policy = json.load(f)
        glob, machine = (release_ages(resolver, c) for c in ({"extends": policy.get("globalExtends", [])}, policy))
        if glob["inheritConfig"] or machine["inheritConfig"]:
            raise ValueError("the machine's Renovate config inherits config from another repo, which could set "
                             "a release age lock-age can't see")
        # Renovate settles which files to look in before it resolves the
        # machine's own extends: from the machine's config as it's written,
        # over its globalExtends'. Set to null, only Renovate's own.
        files = (policy["configFileNames"] if "configFileNames" in policy else glob.get("configFileNames")) or []
        repo = repo_config(files)
        got = [glob, machine, *([release_ages(resolver, repo)] if repo is not None else [])]
        # A maintenance branch for another base reads the default branch's
        # config, which isn't the one checked out here.
        if any(g["baseBranchPatterns"] for g in got):
            raise ValueError("Renovate targets branches besides the default, whose config lock-age can't read")
        ages = [x for g in got for x in g["ages"]]
        age = max(ages, default=0)
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
