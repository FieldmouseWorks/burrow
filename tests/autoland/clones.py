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
ASKED = []


def gh(*a, **k):
    assert a[0] == "api" and a[1].startswith("repos/"), a
    name = a[1][len("repos/"):]
    ASKED.append((C._for[-1], name))
    if C._for[-1] == "old-org":  # the App's long gone from where the repo moved from
        raise RuntimeError("the App steward isn't installed on old-org")
    if name not in NAMES:
        raise RuntimeError("gh api: HTTP 404")
    return {"full_name": NAMES[name]}


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
for x in ("known.json", "baseline-failed.json", C.TAKEN_FILE, "digest-repos.json", "wake.json", "repo-sam_tool.json"):
    if os.path.exists(f"{tmp}/state/{x}"):
        os.remove(f"{tmp}/state/{x}")
ACTING.clear()
print("clones: ok")
