"""The steward's bare clone of each repo, which only it reads: what a
proof's builds and a merge's effect are worked out from (proofs.py, prs.py
`onto`). burrow-upkeep fetches into it (crates/burrow-upkeep/src/mirror.rs)."""

import json
import subprocess

import common as C

# Under STATE: a clone per repo.
GIT = "git"


def git(*args, cwd=None, input=None, env=None):
    p = subprocess.run(["git", *args], cwd=cwd, input=input, env=env, capture_output=True, check=False)
    if p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:2])}: {p.stderr.decode(errors='replace').strip()[:300]}")
    return p.stdout


def fetch(repo, *refs, pr=None):
    """The steward's bare clone of the repo with refs fetched into it from
    GitHub (burrow-upkeep's `fetch`). With pr, its head too, as refs/pr/N:
    from refs/pull/N/head, or by the head's id when GitHub has lost that ref
    (mirror.rs `fetch_pr`). As long as git takes while GitHub keeps
    answering; one that goes quiet for mirror.rs's STALL fails, as any fetch
    that fails."""
    ask = {"refs": list(refs), "pr": pr and {"number": pr["number"], "head": pr["headRefOid"]}}
    out = C.upkeep("fetch", repo, input=json.dumps(ask), bounded=False)
    if out["mirror"] is None:
        raise RuntimeError(out["why"])
    return out["mirror"]
