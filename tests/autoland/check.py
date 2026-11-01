"""The steward, with gh, Claude jobs, GitHub's git remote and the review
agent stood in for: the App it acts as, the reviewers (the snapshot they read, the brief
they're queued with, the reviewers' service taking it, what gets posted), handle() end to end, past the review-round limit,
on a conflict with its base and on dependency updates too, and the reviewers'
answers through it, Claude jobs that end without their answer, what a pass
keeps up in each repo, the branches it deletes, what a PR touches, what starts its passes, the
digest's issues, clones under a repo's old name, burrow-upkeep's looks at Renovate's dashboards and flaky tests, the merge train, that
no failing step loops quietly, and the label that says what a PR waits on.
Each module is one part, run in this order.

  nix eval --json .#nixosConfigurations.example.config.burrow.autoland.oneWay >/tmp/one-way.json
  (cd crates && cargo build -p burrow-review -p burrow-upkeep)
  python3 -I tests/autoland/check.py modules/autoland/steward.py /tmp/one-way.json \
    crates/target/debug/burrow-review crates/burrow-review/tests/fakes \
    crates/target/debug/burrow-upkeep crates/burrow-upkeep/src
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("app", "review", "post", "handle", "rounds", "conflicts", "methods", "slots", "builds", "proving", "docs", "cancelled", "legs", "lost", "superseded", "deps", "answers", "upkeep", "cleanup", "gates", "passes", "digest", "clones", "upkeeping", "watched", "strikes", "trains", "waiting"):
    importlib.import_module(part)
