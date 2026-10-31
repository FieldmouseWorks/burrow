"""A folder in projectsDir that a repo's checkout wants: a clone under the
repo's old name (GitHub answers that name as the repo) is its checkout, and has
its origin repointed; a clone of anything else stays, is reported once on the
stuck issue, and no job tries to clone over it."""

import os
import subprocess
import sys

from harness import REAL, ACTING, C, S, tmp

src = f"{tmp}/clones"
os.makedirs(src)
bin_dir = f"{tmp}/clones-bin"
os.makedirs(bin_dir)
CLONES = f"{tmp}/clones-tried"
# Stand-ins for what a job runs: `gh repo clone` makes the clone, as far as a
# folder with the origin goes, and fails on one that's there, as git does.
with open(f"{bin_dir}/gh", "w") as f:
    f.write(f"""#!/bin/sh
echo "$3" >>{CLONES}
[ -n "$(ls -A "$4" 2>/dev/null)" ] && exit 1
git init -q "$4" && git -C "$4" remote add origin "https://github.com/$3.git"
""")
with open(f"{bin_dir}/proj", "w") as f:
    f.write(f"#!/bin/sh\necho \"$@\" >{tmp}/clones-proj\necho job-1\n")
for x in ("gh", "proj"):
    os.chmod(f"{bin_dir}/{x}", 0o755)

kept_cfg = dict(S.CFG)
kept_path, kept_gh, kept_job, kept_argv = os.environ["PATH"], C.gh, C.job, sys.argv
os.environ["PATH"] = f"{bin_dir}:{kept_path}"
S.CFG.update(projectsDir=src, proj=f"{bin_dir}/proj", owners=["sam"], baselineJobs=True, baselineTask="baseline REPO")
C.job = REAL["job"]

# What GitHub answers: a repo's old names lead to it, as seen by whichever
# owner's installation can; `gone` doesn't exist.
NAMES = {"old-org/app": "sam/app", "sam/was": "sam/now", "eve/app": "eve/app", "sam/app": "sam/app"}
IDS = {}  # the id GitHub answers a current name with, when a check cares
ASKED = []


def gh(*a, **k):
    if a[:2] != ("api", "graphql"):  # a hook's, say: hook_projects goes on without it
        raise RuntimeError("gh api: HTTP 404")
    assert "id: databaseId" in C.NOW_CALLED, "a repo is known by its databaseId, as discover() has it"
    args = dict(x.split("=", 1) for x in a[2:] if "=" in x and not x.startswith("query="))
    name = f"{args['owner']}/{args['name']}"
    ASKED.append((C._for[-1], name))
    if C._for[-1] == "old-org":  # the App's long gone from where the repo moved from
        raise RuntimeError("the App steward isn't installed on old-org")
    if name not in NAMES:
        raise RuntimeError("gh api: Could not resolve to a Repository")
    now = NAMES[name]
    return {"data": {"repository": {"id": IDS.get(now, f"id-{now}"), "nameWithOwner": now, "createdAt": "2026-06-01T00:00:00Z"}}}


C.gh = gh


def clone(folder, url):
    subprocess.run(["git", "init", "-q", f"{src}/{folder}"], check=True)
    subprocess.run(["git", "-C", f"{src}/{folder}", "remote", "add", "origin", url], check=True)


def url(folder):
    return subprocess.run(["git", "-C", f"{src}/{folder}", "remote", "get-url", "origin"],
                          capture_output=True, text=True, check=True).stdout.strip()


def tried():
    return open(CLONES).read().split() if os.path.exists(CLONES) else []


# Under its old name, in the folder a clone would go to (ConaryLabs/nomos in
# nomos, the case this comes from): that's its checkout. Nothing's cloned, the
# job goes there, and the origin says so for all else that reads it.
clone("app", "https://github.com/old-org/app.git")
with C.acting_for("sam/app"):
    assert C.job("sam/app", "t", clone=True) == "job-1"
assert tried() == [], tried()
assert url("app") == "https://github.com/sam/app.git", url("app")
assert C.project_dir("sam/app") == "app"
assert open(f"{tmp}/clones-proj").read().split()[-2:] == ["app", "t"]
assert C.taken("sam/app") is None

# Renamed outright, in a folder of the old name, and over ssh: it keeps its
# folder and its kind of URL, and no second clone is made in `now`.
clone("was", "git@github.com:sam/was.git")
with C.acting_for("sam/now"):
    assert C.job("sam/now", "t", clone=True) == "job-1"
assert url("was") == "git@github.com:sam/now.git", url("was")
assert tried() == [] and not os.path.exists(f"{src}/now")

# A full pass does it before anything needs the checkout, whichever job it is
# (one that doesn't clone can't find it otherwise), and hook_projects then
# hooks the repo by its name. A clone of a repo the pass has seen isn't asked
# about, nor is one whose name is only spelled differently touched.
clone("later", "https://github.com/old-org/later.git")
clone("Spelled", "https://github.com/EVE/app.git")
NAMES.update({"old-org/later": "eve/later", "EVE/app": "eve/app"})
ASKED.clear()
C.follow_renames({"sam/app", "sam/now"})
assert url("later") == "https://github.com/eve/later.git", url("later")
assert url("Spelled") == "https://github.com/EVE/app.git", url("Spelled")
assert not [x for x in ASKED if x[1] in ("sam/app", "sam/now")], ASKED
assert ("sam", "old-org/later") in ASKED, ASKED  # past the owner the App's gone from
hooked = S.R.hook_projects([{"repo": "sam/app"}, {"repo": "sam/now"}])
assert {"repo": "eve/later"} in hooked and {"repo": "old-org/later"} not in hooked, hooked
subprocess.run(["rm", "-rf", f"{src}/later", f"{src}/Spelled"], check=True)
for x in ("repo-eve_later.json", "repo-EVE_app.json", "repo-eve_app.json"):
    if os.path.exists(f"{tmp}/state/{x}"):
        os.remove(f"{tmp}/state/{x}")

# GitHub not answering says nothing about what a repo's called: it stays.
clone("later", "https://github.com/sam/later.git")


def down(*a, **k):
    raise C.Unavailable("gh api: HTTP 502")


C.gh = down
C.follow_renames()
assert url("later") == "https://github.com/sam/later.git"
C.gh = gh
subprocess.run(["rm", "-rf", f"{src}/later"], check=True)

# A name a deleted repo had, that another took on and then renamed away: no
# rename. By the databaseId kept under the old name, or without one (a node id
# kept before says nothing) by when its settings were set. Its clone isn't
# adopted, and it's handed back to be retired; a settings file with the repo's
# own databaseId, or newer than the repo, is a genuine rename's.
MADE = 1780272000  # 2026-06-01T00:00:00Z
for kept_state, gone in (({"settings": MADE + 99, "databaseId": "id-other"}, True), ({"settings": MADE - 99}, True),
                         ({"databaseId": "id-other"}, True), ({"id": "R_kept", "settings": MADE - 99}, True),
                         ({"settings": MADE + 99, "databaseId": "id-sam/new"}, False), ({"settings": MADE + 99}, False),
                         ({"id": "R_kept", "settings": MADE + 99}, False), ({"id": "R_kept"}, False)):
    clone("old", "https://github.com/sam/old.git")
    NAMES["sam/old"] = "sam/new"
    C.save("repo-sam_old.json", kept_state)
    C.follow_renames()
    NAMES["sam/other"] = "sam/other"
    assert C.retiring("sam/new") == gone, kept_state
    assert not C.retiring("sam/other")
    if gone:
        # The hold follows the repo that took the name to its next one, by its
        # id, and not to another that takes up the name it left. Not knowing
        # what a repo is holds it too.
        NAMES["sam/latest"] = "sam/latest"
        IDS["sam/latest"] = "id-sam/new"
        assert C.retiring("sam/latest")
        NAMES["sam/new"] = "sam/new"
        IDS["sam/new"] = "id-unrelated"
        assert not C.retiring("sam/new")
        IDS.clear()
        del NAMES["sam/new"]
        assert C.retiring("sam/nobody")
    C.save(C.RETIRING_FILE, {})
    assert (url("old") == "https://github.com/sam/old.git") == gone, (kept_state, url("old"))
    subprocess.run(["rm", "-rf", f"{src}/old"], check=True)
    os.remove(f"{tmp}/state/repo-sam_old.json")

# A job can be what finds such a name, before any pass has: no clone for it.
clone("old", "https://github.com/sam/old.git")
C.save("repo-sam_old.json", {"settings": MADE + 99, "databaseId": "id-other"})
with C.acting_for("sam/new"):
    assert C.job("sam/new", "t", clone=True) is None
assert "sam/new" not in tried() and not os.path.exists(f"{src}/new"), tried()
C.save(C.RETIRING_FILE, {})
# Nor when that same look also found it a clone under another old name.
NAMES["sam/alias"] = "sam/new"
clone("alias", "https://github.com/sam/alias.git")
with C.acting_for("sam/new"):
    assert C.job("sam/new", "t", clone=True) is None
assert url("alias") == "https://github.com/sam/new.git", url("alias")
subprocess.run(["rm", "-rf", f"{src}/alias"], check=True)
C.save(C.RETIRING_FILE, {})
subprocess.run(["rm", "-rf", f"{src}/old"], check=True)
os.remove(f"{tmp}/state/repo-sam_old.json")

# A free path is cloned to, an empty folder included, as git would.
os.makedirs(f"{src}/fresh")
with C.acting_for("sam/fresh"):
    assert C.job("sam/fresh", "t", clone=True) == "job-1"
assert tried() == ["sam/fresh"], tried()
os.remove(CLONES)


# A clone of another repo, in the path: reported through a real pass, on the
# stuck issue and not just the log, naming the folder and both origins.
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects)
PUBLISHED = []
NEW = {"new": True}
S.R.discover = lambda: [{"repo": "sam/tool", "default": "main"}]
S.R.settle_repo = lambda r, known: NEW["new"]
S.R.publish = PUBLISHED.append
S.R.unhook_gone = lambda *a: None
S.R.hook_projects = lambda repos: []
S.open_prs = lambda owner, name: ({"id": "R", "defaultBranchRef": {"name": "main"}}, [])
clone("tool", "https://github.com/eve/tool.git")
NAMES["eve/tool"] = "eve/tool"


def full(at, kick=None):
    C.NOW = at
    C.WAKE.clear()
    PUBLISHED.clear()
    sys.argv = ["steward"] + (["kick", kick] if kick else [])
    S.main()
    return [x for x in PUBLISHED[0] if x[0] == C.TAKEN]


T = 2_000_000_000
line = full(T)
assert len(line) == 1 and line[0][1] == "sam/tool", PUBLISHED
for said in (f"{src}/tool", "eve/tool", "sam/tool"):
    assert said in line[0][2], line
assert tried() == [], tried()
assert url("tool") == "https://github.com/eve/tool.git"
# Said once: the same line in later passes, a kicked one too (which replaces
# the repo's lines) and a day on, with no clone tried and no daily retry.
# A full pass repoints a renamed clone by itself, here with no job looking for
# a checkout.
clone("swept", "https://github.com/old-org/swept.git")
NAMES["old-org/swept"] = "sam/swept"
ASKED.clear()
assert full(T + 3600) == line
assert url("swept") == "https://github.com/sam/swept.git", url("swept")
assert full(T + 3700, "sam/tool") == line
assert full(T + 90000) == line
assert tried() == [] and "sam/tool" not in C.load("known.json", [])
assert "sam/tool" not in C.load("baseline-failed.json", {})
# A job for it that doesn't come from the baseline (a dashboard's) gets no
# further, and asks GitHub nothing more.
ASKED.clear()
with C.acting_for("sam/tool"):
    assert C.job("sam/tool", "t", clone=True) is None
assert tried() == [], tried()
assert ASKED == [], ASKED
# What's kept only counts while it's what blocks the clone: another
# projectsDir has none of it, and neither has a folder emptied of what's in it.
S.CFG["projectsDir"] = f"{tmp}/clones-elsewhere"
assert C.taken("sam/tool") is None
# Not even with the same sort of clone in the way there: that's a new folder to
# say, which the record of the old one doesn't.
subprocess.run(["git", "init", "-q", f"{tmp}/clones-elsewhere/tool"], check=True)
subprocess.run(["git", "-C", f"{tmp}/clones-elsewhere/tool", "remote", "add", "origin", "https://github.com/eve/tool.git"], check=True)
assert C.taken("sam/tool") is None
S.CFG["projectsDir"] = src
os.makedirs(f"{src}/plain/stuff")
C.save(C.TAKEN_FILE, {**C.load(C.TAKEN_FILE, {}), "sam/plain": {"folder": f"{src}/plain", "origin": None}})
assert C.taken("sam/plain")
os.rmdir(f"{src}/plain/stuff")
assert C.taken("sam/plain") is None
os.rmdir(f"{src}/plain")
# A full pass hands the deleted repo's name to repos.reset, on its own, and
# goes on when that fails, listing it. A try that moved the clone and then
# couldn't stop a job is tried again though there's no clone left to find it
# by, and the repo that took the name gets no job until it's done, not one a
# checkout would make.
RESET = []
RESET_FAILS = [True]


def reset(r, k, known, failed):
    RESET.append((r["repo"], k))
    if RESET_FAILS[0]:
        raise RuntimeError("proj won't stop its job")


kept_reset, S.R.reset = S.R.reset, reset
clone("old", "https://github.com/sam/old.git")
C.save("repo-sam_old.json", {"settings": T, "databaseId": "id-other"})
assert full(T + 90050) == line
assert RESET == [("sam/old", "sam_old")], RESET
assert url("old") == "https://github.com/sam/old.git"
assert [x for x in PUBLISHED[0] if x[0] == C.RETIRING and x[1] == "sam/old" and "sam/new" in x[2]], PUBLISHED
with C.acting_for("sam/new"):
    assert C.job("sam/new", "t", clone=True) is None
assert "sam/new" not in tried(), tried()
# Nor in a checkout of its own that it has already.
clone("new", "https://github.com/sam/new.git")
with C.acting_for("sam/new"):
    assert C.job("sam/new", "t") is None
subprocess.run(["rm", "-rf", f"{src}/old", f"{src}/new"], check=True)
assert full(T + 90060) == line
assert len(RESET) == 2, RESET
RESET_FAILS[0] = False
assert full(T + 90070) == line
assert len(RESET) == 3 and not C.retiring("sam/new"), RESET
assert not [x for x in PUBLISHED[0] if x[0] == C.RETIRING], PUBLISHED
# Done with its state gone already: a try that stopped just short of saying so.
C.save(C.RETIRING_FILE, {"sam_gone": {"name": "sam/gone", "for": "sam/took", "id": "id-sam/took"}})
full(T + 90075)
assert len(RESET) == 3 and not C.retiring("sam/took"), RESET
S.R.reset = kept_reset
os.remove(f"{tmp}/state/repo-sam_old.json")
# A hidden folder counts only while it's named for its repo. Transferred, it
# still is. Renamed, it's repointed where it stands, for any job with its path,
# and the repo's next job clones it afresh.
clone(".cfg", "https://github.com/sam/.cfg.git")
NAMES["sam/.cfg"] = "eve/.cfg"
C.follow_renames()
assert C.project_dir("eve/.cfg") == ".cfg" and url(".cfg") == "https://github.com/eve/.cfg.git", url(".cfg")
NAMES["eve/.cfg"] = "sam/config"
NAMES["sam/config"] = "sam/config"
C.follow_renames()
assert url(".cfg") == "https://github.com/sam/config.git", url(".cfg")
with C.acting_for("sam/config"):
    assert C.job("sam/config", "t", clone=True) == "job-1"
assert C.project_dir("sam/config") == "config" and os.path.isdir(f"{src}/.cfg")
subprocess.run(["rm", "-rf", f"{src}/.cfg", f"{src}/config"], check=True)
os.remove(CLONES)
# Moved out by hand: the line's gone, and the next pass clones it and goes.
subprocess.run(["mv", f"{src}/tool", f"{tmp}/clones-moved"], check=True)
assert full(T + 90100) == []
assert tried() == ["sam/tool"], tried()
assert "sam/tool" in C.load("known.json", [])

S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects = kept_fns
C.gh, C.job, sys.argv = kept_gh, kept_job, kept_argv
os.environ["PATH"] = kept_path
S.CFG.clear()
S.CFG.update(kept_cfg)
for x in ("known.json", "baseline-failed.json", C.TAKEN_FILE, C.RETIRING_FILE, "digest-repos.json", "wake.json", "repo-sam_tool.json"):
    if os.path.exists(f"{tmp}/state/{x}"):
        os.remove(f"{tmp}/state/{x}")
ACTING.clear()
print("clones: ok")
