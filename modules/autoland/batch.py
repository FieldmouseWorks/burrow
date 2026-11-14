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
        self.kick = None if kick is None else {r.lower() for r in kick}
        self.advanced = False
        start = time.monotonic()
        self.until = start + SECONDS
        self.hard_until = start + C.PASS_SECONDS
        self.state = C.load(FILE, {})
        self.state.setdefault("repos", {})
        self.state.setdefault("finished", [])
        self.state.setdefault("owed", [])  # kicked repos not yet entered or finished
        self.state["repos"] = {k.lower(): v for k, v in self.state["repos"].items()}
        for st in self.state["repos"].values():
            # Old checkpoints did not identify which PR held the train.
            if why := st.pop("stopped", None):
                st["stops"] = dict.fromkeys(st.get("done", {}), why)
                st["done"] = dict.fromkeys(st.get("done", {}))
            if not st.pop("seen_to", True):
                st["unseen"] = list(st.get("done", {}))
                st["done"] = dict.fromkeys(st.get("done", {}))
        for key in ("finished", "owed"):
            self.state[key] = sorted({r.lower() for r in self.state[key]})
        self.state["retry"] = {k.lower(): v for k, v in self.state.get("retry", {}).items()}
        self.done = set()
        self.left = False  # PRs or repos still unvisited, not only retries
        self.later = None  # when an unavailable operation may be retried
        self.traversal_full = self.state.get("full", False) or (self.full and not continuing)

    def repos(self, repos):
        names = {r["repo"].lower() for r in repos}
        self.state["repos"] = {k: v for k, v in self.state["repos"].items() if k in names}
        for key in ("finished", "owed"):
            self.state[key] = [k for k in self.state[key] if k in names]
        self.state["retry"] = {k: at for k, at in self.state["retry"].items() if k in names}
        if self.kick is not None:
            repos = [r for r in repos if r["repo"].lower() in self.kick]
        candidates = self.state["repos"].keys() | set(self.state["owed"])
        if self.full and self.continuing and not self.traversal_full:
            repos = [r for r in repos if r["repo"].lower() in candidates]
        else:
            repos = [r for r in repos if not self.full or r["repo"].lower() not in self.state["finished"]]
        ready = []
        limited = C.load(C.RETRY_AFTER, 0)
        for r in repos:
            repo = r["repo"].lower()
            at = max(self.state["retry"].get(repo, 0), limited)
            if at > C.NOW:
                self.later = min(self.later or at, at)
                C.wake(r["repo"], at)  # ends: retry's deadline
            else:
                ready.append(r)
        # A slow failure can be due again by the next batch. Ready work
        # that has not failed gets its turn before another retry attempt.
        ready.sort(key=lambda r: self.state["retry"].get(r["repo"].lower(), 0))
        return ready

    def active(self, kick):
        if not self.continuing:
            return True
        if kick is None:
            return self.traversal_full or bool(self.state["repos"] or self.state["owed"])
        return bool({r.lower() for r in kick} & (self.state["repos"].keys() | set(self.state["owed"])))

    def expired(self):
        now = time.monotonic()
        if not self.advanced:
            # Slow preparation gets a useful work slice, bounded by the
            # remote deadline; it must not cause repeated zero-work passes.
            if now >= self.until:
                self.until = min(now + SECONDS, self.hard_until)
            return False
        return now >= self.until

    def retry(self, repo, at):
        self.state["retry"][repo.lower()] = at
        self.advanced = True  # an unsuccessful attempt still spends the slice
        self.later = at if self.later is None else min(self.later, at)

    def digest(self, repo):
        return list(self.state["repos"].get(repo.lower(), {}).get("digest", []))

    def pending(self, repo, prs, mine, info):
        st = self.state["repos"].get(repo.lower(), {})
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
        live = {str(pr["number"]) for pr in prs}
        st["stops"] = {k: v for k, v in st.get("stops", {}).items() if k in live}
        st["unseen"] = [k for k in st.get("unseen", []) if k in live]
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
        st = self.state["repos"].get(repo.lower(), {})
        key = str(pr["number"])
        return st.get("visits", {}).get(key, 1) if key in st.get("done", {}) else 0

    def stopped(self, repo):
        return next(iter(self.state["repos"].get(repo.lower(), {}).get("stops", {}).values()), None)

    def seen_to(self, repo):
        return not self.state["repos"].get(repo.lower(), {}).get("unseen")

    def note(self, repo, pr, mine, seen_to, info, stopped=None):
        st = self.state["repos"].setdefault(repo.lower(), {"done": {}})
        st["sequence"] = st.get("sequence", 1) + 1
        st.setdefault("visits", {})[str(pr["number"])] = st["sequence"]
        st["done"][str(pr["number"])] = self.rev(pr, info)
        st["digest"] = mine
        key = str(pr["number"])
        st.setdefault("stops", {}).pop(key, None)
        if stopped:
            st["stops"][key] = stopped
        unseen = set(st.get("unseen", [])) - {key}
        if not seen_to:
            unseen.add(key)
        st["unseen"] = sorted(unseen)
        st["wake"] = C.WAKE.get(repo)
        self.advanced = True

    def listed(self, repo, op, mine, lines):
        """What the repo's operation op lists now replaces, in mine, what it
        listed in an earlier batch of this traversal (kept here until the
        traversal ends)."""
        st = self.state["repos"].setdefault(repo.lower(), {"done": {}})
        ops = st.setdefault("operations", {})
        old = {tuple(x) for x in ops.get(op, ())}
        lines = list(dict.fromkeys(tuple(x) for x in lines))
        mine[:] = [x for x in mine if tuple(x) not in old and tuple(x) not in lines] + lines
        ops[op] = [list(x) for x in lines]
        st["digest"] = mine

    def finish(self, repo, seen=True):
        """The repo is done with for this traversal. Its PR cursor goes too,
        unless the look failed before it had seen them all (not seen): the
        look that retries it then goes on with the PRs no slice has visited,
        where one starting over left the last of a long list without a look
        for as long as the read failed now and then."""
        self.advanced = True
        self.done.add(repo.lower())
        repo = repo.lower()
        if seen:
            self.state["repos"].pop(repo, None)
        self.state["retry"].pop(repo, None)
        self.state["owed"] = [r for r in self.state["owed"] if r != repo]
        if self.traversal_full and repo not in self.state["finished"]:
            self.state["finished"].append(repo)

    def end(self, complete, kick):
        if C.LIMITED and C.LIMITED[0] > C.NOW:
            at = C.LIMITED[0] + 1
            targets = set(kick or self.state["repos"]) | set(self.state["owed"])
            self.state["retry"].update(dict.fromkeys(targets, at))
            self.left = False
            self.later = at
        if self.later:
            complete = False
        if self.full and self.traversal_full:
            self.state["full"] = not complete
            if complete:
                self.state["finished"] = []
        if kick is not None:
            # A bounded multi-repo kick owes every target it didn't finish,
            # including ones it never entered and so have no cursor.
            owed = set(self.state["owed"]) | {r.lower() for r in kick}
            self.state["owed"] = sorted(owed - self.done if not complete else owed - {r.lower() for r in kick})
        self.state["owed"] = sorted(set(self.state["owed"]) - self.done)
        C.save(FILE, self.state)
        # Continuations have their own event line: they are not new-work
        # requests and must not consume GitHub events or decision deadlines.
        wakes = C.load("continue.json", {})
        again = [""] if kick is None else kick
        wakes = {k: at for k, at in wakes.items() if k.lower() not in self.done and k.lower() not in again}
        for key in again:
            if not complete:
                # Retries wait for their own deadline; only unvisited work is due at once.
                wakes[key] = self.state["retry"].get(key.lower(), C.NOW + 1 if self.left or not self.later else self.later)
        if C.LIMITED and C.LIMITED[0] > C.NOW:
            wakes = {k: max(v, C.LIMITED[0] + 1) for k, v in wakes.items()}
        C.save("continue.json", wakes)
