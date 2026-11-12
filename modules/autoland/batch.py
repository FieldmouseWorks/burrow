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
SECONDS = 8 * C.MIN  # twelve minutes remain for one step and publishing state


class Batch:
    def __init__(self, kick, continuing=False):
        self.full = kick is None
        self.continuing = continuing
        self.advanced = False
        self.until = time.monotonic() + SECONDS
        self.state = C.load(FILE, {})
        self.state.setdefault("repos", {})
        self.state.setdefault("finished", [])

    def repos(self, repos):
        names = {r["repo"] for r in repos}
        self.state["repos"] = {k: v for k, v in self.state["repos"].items() if k in names}
        self.state["finished"] = [k for k in self.state["finished"] if k in names]
        return [r for r in repos if not self.full or r["repo"] not in self.state["finished"]]

    def active(self, kick):
        if not self.continuing:
            return True
        return self.state.get("full", False) if kick is None else any(r.lower() in kick for r in self.state["repos"])

    def expired(self):
        # Slow discovery/preparation must not prevent the first durable step.
        # main's remote deadline still reserves time before systemd's limit.
        return self.advanced and time.monotonic() >= self.until

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
        return str(pr["number"]) in self.state["repos"].get(repo, {}).get("done", {})

    def stopped(self, repo):
        return self.state["repos"].get(repo, {}).get("stopped")

    def seen_to(self, repo):
        return self.state["repos"].get(repo, {}).get("seen_to", True)

    def note(self, repo, pr, mine, seen_to, info, stopped=None):
        st = self.state["repos"].setdefault(repo, {"done": {}})
        st["done"][str(pr["number"])] = self.rev(pr, info)
        st["digest"] = mine
        st["seen_to"] = seen_to
        st["wake"] = C.WAKE.get(repo)
        st["stopped"] = stopped
        self.advanced = True

    def finish(self, repo):
        self.advanced = True
        self.state["repos"].pop(repo, None)
        if (self.full or self.state.get("full")) and repo not in self.state["finished"]:
            self.state["finished"].append(repo)

    def end(self, complete, kick):
        if self.full:
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
