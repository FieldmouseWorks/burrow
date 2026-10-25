"""The steward, with gh, Claude jobs, GitHub's git remote and the review
agents stood in for: the reviewers (the snapshot they read, the service that
runs them, what gets posted), handle() end to end, what a PR touches, and
what starts its passes. Each module is one part, run in this order.

  nix eval --json .#nixosConfigurations.example.config.burrow.autoland.oneWay >/tmp/one-way.json
  python3 -I tests/autoland/check.py modules/autoland/steward.py /tmp/one-way.json
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("review", "post", "handle", "gates", "passes"):
    importlib.import_module(part)
