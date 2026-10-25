"""The steward, with gh, Claude jobs and DeepSeek's API stood in for: its
DeepSeek reviewer, handle() end to end, and what a PR touches. Each module
is one part, run in this order.

  nix eval --json .#nixosConfigurations.example.config.burrow.autoland.oneWay >/tmp/one-way.json
  python3 -I tests/autoland/check.py modules/autoland/steward.py /tmp/one-way.json
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("deepseek_review", "handle", "gates"):
    importlib.import_module(part)
