"""The steward, with gh, agent jobs and GitHub's git remote stood in for:
the App it acts as, handle() end to end on Codex's connector's posts, the
briefs as they were before their text moved to burrow-brief, a judge that
fails a head on its code or its description alone, past the review-round
limit, on a conflict with its base and on dependency updates too, a PR whose merge would change nothing, one stacked on another's branch, agent jobs that end without their answer, a fix job's answers (posted only on its own push), a red check's cause tracked outside the PR, what a pass
keeps up in each repo, what a PR touches, what starts its passes, the
digest's issues, clones under a repo's old name, burrow-upkeep's looks at a repo's settings, its branches, Renovate's dashboards and
flaky tests, the merge train, that
no failing step loops quietly, and the label that says what a PR waits on.
Each module is one part, run in this order. Needs git, bash, jq and openssl
on PATH, as checks/autoland.nix supplies them.

  policy=$(nix eval --raw .#nixosConfigurations.example.config.burrow.ledger.env.CONARY_POLICY)
  (cd crates && cargo build -p burrow-upkeep -p burrow-proof -p burrow-settings -p burrow-brief)
  python3 -I tests/autoland/check.py modules/autoland/steward.py "$policy" \
    crates/target/debug/burrow-upkeep crates/burrow-upkeep/src \
    crates/target/debug/burrow-proof crates/burrow-proj/src/ask.rs \
    crates/target/debug/burrow-settings \
    crates/target/debug/burrow-brief crates/burrow-brief/tests/golden
  nix build .#checks.x86_64-linux.autoland
"""

import importlib
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402,F401 (the steward, set up from argv)

for part in ("app", "handle", "owner_asks", "briefs", "misdescribed", "rounds", "merits", "conflicts", "methods", "landed", "slots", "builds", "proving", "runs", "docs", "cancelled", "rerun", "unstarted", "stacked", "legs", "lost", "unpushed", "tracked", "superseded", "deps", "upkeep", "undos", "alone", "gates", "passes", "digest", "clones", "upkeeping", "watched", "strikes", "trains", "waiting", "paused", "handoff", "landing_owner"):
    importlib.import_module(part)
