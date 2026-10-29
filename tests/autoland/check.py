"""The steward, with gh, Claude jobs, GitHub's git remote and the review
agent stood in for: the App it acts as, the reviewers (the snapshot they read, the service that
runs them, what gets posted), handle() end to end, past the review-round limit,
on a conflict with its base and on dependency updates too, and the reviewers'
answers through it, what a pass keeps up in each repo, what a PR touches,
what starts its passes, the digest's issues, Renovate's dashboards and flaky
tests, and that no failing step loops quietly.
Each module is one part, run in this order.

  nix eval --json .#nixosConfigurations.example.config.burrow.autoland.oneWay >/tmp/one-way.json
  python3 -I tests/autoland/check.py modules/autoland/steward.py /tmp/one-way.json
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("app", "review", "post", "handle", "rounds", "conflicts", "slots", "deps", "answers", "upkeep", "gates", "passes", "digest", "dashboards", "watched", "quarantines", "strikes"):
    importlib.import_module(part)
