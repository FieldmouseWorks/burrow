"""autoland-steward against a stand-in for GitHub: the labels it reads.

    python3 test_steward.py path/to/steward.py
"""

import importlib.util
import json
import os
import sys
import tempfile
import unittest

STEWARD = sys.argv.pop(1)
HEAD = "a" * 40
CODEX = "chatgpt-codex-connector"


class GitHub:
    """The few gh calls these paths make, against one repo's labels and one
    PR's. `creates` False: the token can't create labels."""

    def __init__(self, labels=(), creates=True):
        self.labels = set(labels)
        self.pr_labels = set()
        self.creates = creates
        self.calls = []

    def gh(self, *args, input=None, check=True):
        self.calls.append(args)
        if args[:2] == ("label", "list"):
            return [{"name": x} for x in sorted(self.labels)]
        if args[:2] == ("label", "create"):
            if not self.creates:
                return None
            self.labels.add(args[2])
            return ""
        if args[:2] == ("pr", "edit") and "--add-label" in args:
            label = args[args.index("--add-label") + 1]
            if label not in self.labels:
                return None  # gh: "'needs-peter' not found"
            self.pr_labels.add(label)
            return ""
        if args[:2] == ("api", "repos/o/r"):
            return {"allow_auto_merge": True, "delete_branch_on_merge": True, "allow_squash_merge": True}
        return ""

    def comments(self):
        return [a for a in self.calls if a[0] == "api" and a[1].endswith("/comments")]

    def pr(self):
        """The PR as the steward's query returns it: five Codex rounds on
        this head, every one with findings."""
        def nodes(xs):
            return {"pageInfo": {"hasNextPage": False, "hasPreviousPage": False}, "nodes": xs}
        review = {"author": {"login": CODEX}, "state": "COMMENTED", "body": "a finding",
                  "submittedAt": "2026-10-24T03:00:00Z", "commit": {"oid": HEAD}}
        return {
            "id": "PR_1", "number": 81, "title": "a change", "url": "https://github.com/o/r/pull/81",
            "isDraft": True, "isCrossRepository": False, "headRefName": "claude/x", "headRefOid": HEAD,
            "baseRefName": "main", "createdAt": "2026-10-24T00:00:00Z", "updatedAt": "2026-10-24T03:00:00Z",
            "mergeStateStatus": "CLEAN", "author": {"login": "sam"}, "autoMergeRequest": None,
            "labels": nodes([{"name": x} for x in sorted(self.pr_labels)]),
            "files": nodes([{"path": "a.py"}]), "changedFiles": 1,
            "reviewThreads": nodes([]), "reviews": nodes([review] * 5),
            "comments": nodes([]), "reactions": nodes([]),
            "commits": nodes([{"commit": {"oid": HEAD, "statusCheckRollup": {"contexts": nodes([])}}}]),
        }


class Steward(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        cfg = {
            "context": "review-settled", "codexLogin": CODEX, "stateDir": self.dir.name,
            "owners": ["o"], "authors": ["sam"], "exclude": [], "rulesets": False,
            "renovateEmail": None, "projectsDir": self.dir.name, "proj": "false", "owner": "sam",
            "labels": {"hold": "hold", "owner": "needs-peter"},
            "notify": {"repo": None, "assignee": None, "label": "updates"},
            "defaults": {"rounds": 5, "oneWay": [], "handoffAfter": 30, "settleDelay": 5, "codexTimeout": 30,
                         "directPush": False, "ruleset": True, "requireCi": True, "actionBumps": True,
                         "ownerChecks": []},
            "repos": {},
        }
        path = os.path.join(self.dir.name, "config.json")
        with open(path, "w") as f:
            json.dump(cfg, f)
        os.environ["AUTOLAND_CONFIG"] = path
        spec = importlib.util.spec_from_file_location("steward", STEWARD)
        self.s = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.s)

    def tearDown(self):
        self.dir.cleanup()

    def use(self, github):
        self.s.gh = github.gh
        return github

    def state(self):
        return json.load(open(os.path.join(self.dir.name, "pr-o_r-81.json")))

    def passes(self, github, n):
        for _ in range(n):
            self.s.handle("o/r", github.pr(), {"squashMergeAllowed": True}, [])

    def test_settle_repo_makes_the_labels(self):
        github = self.use(GitHub(labels={"bug"}))
        self.s.settle_repo({"repo": "o/r", "private": True, "default": "main"}, set())
        self.assertEqual(github.labels, {"bug", "hold", "needs-peter"})

    def test_settle_repo_keeps_labels_the_repo_has(self):
        github = self.use(GitHub(labels={"Hold", "needs-peter"}))
        self.s.settle_repo({"repo": "o/r", "private": True, "default": "main"}, set())
        self.assertFalse([a for a in github.calls if a[:2] == ("label", "create")])

    def test_settle_repo_makes_labels_new_to_the_config_before_a_day_is_up(self):
        github = self.use(GitHub())
        r = {"repo": "o/r", "private": True, "default": "main"}
        self.s.settle_repo(r, set())
        self.s.CFG["labels"]["hold"] = "paused"
        self.s.settle_repo(r, set())
        self.assertIn("paused", github.labels)

    def test_round_limit_on_a_repo_without_the_label(self):
        # The label is made at the round limit, put on, and stays the
        # owner's: the next passes don't start the rounds over.
        github = self.use(GitHub())
        self.passes(github, 3)
        self.assertEqual(github.pr_labels, {"needs-peter"})
        self.assertTrue(self.state()["escalated"])
        self.assertEqual(self.state()["base"], 0)
        self.assertEqual(len(github.comments()), 1)

    def test_round_limit_when_the_label_cant_be_made(self):
        # Not escalated while the label isn't on, so a missing label never
        # reads as the owner taking it off.
        github = self.use(GitHub(creates=False))
        self.passes(github, 3)
        self.assertEqual(github.pr_labels, set())
        self.assertFalse(self.state().get("escalated"))
        self.assertEqual(self.state()["base"], 0)
        self.assertEqual(github.comments(), [])

    def test_owner_taking_the_label_off_gives_more_rounds(self):
        github = self.use(GitHub(labels={"needs-peter"}))
        self.passes(github, 1)
        github.pr_labels.clear()
        self.passes(github, 1)
        self.assertEqual(self.state()["base"], 5)


if __name__ == "__main__":
    unittest.main(verbosity=2)
