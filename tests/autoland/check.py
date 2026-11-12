"""The steward, with gh, Claude jobs, GitHub's git remote and the review
agent stood in for: the App it acts as, the reviewers (the snapshot they read, the brief
they're queued with, the reviewers' service taking it, what gets posted), handle() end to end, a judge that fails a head on its code or its description alone, past the review-round limit,
a clean look carried to a head that only merged its base in,
on a conflict with its base and on dependency updates too, and the reviewers'
answers through it, a PR whose merge would change nothing, one stacked on another's branch, Claude jobs that end without their answer, a fix job's answers (posted only on its own push), what a pass
keeps up in each repo, what a PR touches, what starts its passes, the
digest's issues, clones under a repo's old name, burrow-upkeep's looks at a repo's settings, its branches, Renovate's dashboards and
flaky tests, the merge train, that
no failing step loops quietly, and the label that says what a PR waits on.
Each module is one part, run in this order. Needs git, bash, jq and openssl
on PATH, as checks/autoland.nix supplies them.

  policy=$(nix eval --raw .#nixosConfigurations.example.config.burrow.ledger.env.CONARY_POLICY)
  (cd crates && cargo build -p burrow-review -p burrow-upkeep -p burrow-proof -p burrow-settings)
  python3 -I tests/autoland/check.py modules/autoland/steward.py "$policy" \
    crates/target/debug/burrow-review crates/burrow-review/tests/fakes \
    crates/target/debug/burrow-upkeep crates/burrow-upkeep/src \
    crates/target/debug/burrow-proof crates/burrow-proj/src/ask.rs \
    crates/target/debug/burrow-settings
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("app", "review", "post", "handle", "misdescribed", "codex_heads", "rounds", "carried", "conflicts", "methods", "landed", "slots", "builds", "proving", "runs", "docs", "cancelled", "stacked", "legs", "lost", "unpushed", "superseded", "deps", "answers", "upkeep", "undos", "alone", "gates", "passes", "digest", "clones", "upkeeping", "watched", "strikes", "trains", "waiting", "paused"):
    importlib.import_module(part)
