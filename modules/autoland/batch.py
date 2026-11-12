"""A pass yields before systemd kills it, and resumes the work it has left.

The cursor is saved after a batch publishes its waits and wake-ups, so a
continuation cannot skip a PR whose wait was lost. Full passes also remember
completed repos. Kicked passes share a repo's PR cursor, so either kind can finish its batch. No verdict is inferred from a
cursor: train steps and coverage still require seeing every PR in the repo.
"""

import copy
import hashlib
import json
import time

import common as C

FILE = "pass-progress.json"
SECONDS = 8 * C.MIN  # soft work slice, also bounded by C.PASS_SECONDS


class Batch:
    def __init__(self, kick, continuing=False):
        self.full = kick is None
        self.continuing = continuing
        self.advanced = False
        start = time.monotonic()
        self.until = start + SECONDS
        self.hard_until = start + C.PASS_SECONDS
        self.state = C.load(FILE, {})
        self.state.setdefault("repos", {})
        self.state.setdefault("finished", [])
        self.traversal_full = self.state.get("full", False) or (self.full and not continuing)

    def repos(self, repos):
        names = {r["repo"] for r in repos}
        self.state["repos"] = {k: v for k, v in self.state["repos"].items() if k in names}
        self.state["finished"] = [k for k in self.state["finished"] if k in names]
        if self.full and self.continuing and not self.traversal_full:
            # The event runner's startup full catch-up also consumes due
            # per-repo continuations. Resume those cursors, not the fleet.
            return [r for r in repos if r["repo"] in self.state["repos"]]
        return [r for r in repos if not self.full or r["repo"] not in self.state["finished"]]

    def active(self, kick):
        if not self.continuing:
            return True
        if kick is None:
            return self.traversal_full or bool(self.state["repos"])
        return any(r.lower() in kick for r in self.state["repos"])

    def expired(self):
        now = time.monotonic()
        if not self.advanced:
            # Slow preparation gets a useful work slice, bounded by the
            # remote deadline; it must not cause repeated zero-work passes.
            if now >= self.until:
                self.until = min(now + SECONDS, self.hard_until)
            return False
        return now >= self.until

    def digest(self, repo):
        return list(self.state["repos"].get(repo, {}).get("digest", []))

    def pending(self, repo, prs, mine, info):
        st = self.state["repos"].get(repo, {})
        done = st.get("done", {})
        elapsed = st.get("wake") and st["wake"] <= C.NOW
        if not self.continuing or elapsed:
            # New work and elapsed waits invalidate every decision, including
            # local reviewer/proof/job inputs and sibling PR relationships.
            # Retain visited keys so unvisited work still goes first.
            done.update(dict.fromkeys(done))
        if elapsed:
            st.pop("wake", None)
        elif st.get("wake"):
            C.wake(repo, st["wake"])  # ends: the carried PR deadline gets its next look after this traversal
        pending = [pr for pr in prs if done.get(str(pr["number"])) != self.rev(pr, info)]
        # A closed or changed PR's old owner line mustn't suppress its new
        # watchdog wait. Repo-wide notices survive until its traversal ends.
        keep = {f"[{repo}#{pr['number']}](" for pr in prs if pr not in pending}
        mine[:] = [x for x in mine if not x[1].startswith(f"[{repo}#") or any(x[1].startswith(k) for k in keep)]
        return pending

    @staticmethod
    def rev(pr, info):
        # Revisit anything whose decision inputs moved, including a new hold,
        # finding or red check. Our own stage label/status aren't inputs and
        # would invalidate every checkpoint immediately after writing it.
        value = copy.deepcopy(pr)
        value.pop("updatedAt", None)
        waiting = {v["name"] for v in C.CFG["labels"]["waiting"].values()}
        if labels := value.get("labels"):
            labels["nodes"] = [x for x in labels.get("nodes", []) if x.get("name") not in waiting]
        for node in value.get("commits", {}).get("nodes", []):
            contexts = ((node["commit"].get("statusCheckRollup") or {}).get("contexts") or {})
            contexts["nodes"] = [x for x in contexts.get("nodes", []) if x.get("context") != C.CONTEXT]
        return hashlib.sha256(json.dumps([value, info, C.CFG], sort_keys=True).encode()).hexdigest()

    def visited(self, repo, pr):
        st = self.state["repos"].get(repo, {})
        key = str(pr["number"])
        return st.get("visits", {}).get(key, 1) if key in st.get("done", {}) else 0

    def stopped(self, repo):
        return self.state["repos"].get(repo, {}).get("stopped")

    def seen_to(self, repo):
        return self.state["repos"].get(repo, {}).get("seen_to", True)

    def note(self, repo, pr, mine, seen_to, info, stopped=None):
        st = self.state["repos"].setdefault(repo, {"done": {}})
        st["sequence"] = st.get("sequence", 1) + 1
        st.setdefault("visits", {})[str(pr["number"])] = st["sequence"]
        st["done"][str(pr["number"])] = self.rev(pr, info)
        st["digest"] = mine
        st["seen_to"] = seen_to
        st["wake"] = C.WAKE.get(repo)
        st["stopped"] = stopped
        self.advanced = True

    def finish(self, repo):
        self.advanced = True
        self.state["repos"].pop(repo, None)
        if self.traversal_full and repo not in self.state["finished"]:
            self.state["finished"].append(repo)

    def end(self, complete, kick):
        if self.full and self.traversal_full:
            self.state["full"] = not complete
            if complete:
                self.state["finished"] = []
        C.save(FILE, self.state)
        # Continuations have their own event line: they are not new-work
        # requests and must not consume GitHub events or decision deadlines.
        wakes = C.load("continue.json", {})
        again = [""] if kick is None else kick
        for key in again:
            wakes.pop(key, None)
            if not complete:
                wakes[key] = C.NOW + 1
        C.save("continue.json", wakes)
