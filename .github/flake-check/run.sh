# Only bootstrap glue. The policy lives in crates/burrow-flake-check.
# Build (or substitute from the build store) the workflow's pinned runner
# package with Nix, keeping its store path rooted while it runs. Never reuse
# an executable a preceding step could have left in RUNNER_TEMP.
set -eo pipefail
here=$(cd -- "${BASH_SOURCE[0]%/*}" && pwd)
repo=$(cd -- "$here/../.." && pwd)
work=$(mktemp -d "$RUNNER_TEMP/runner.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
started=$SECONDS
nix build --extra-experimental-features 'nix-command flakes' --no-update-lock-file --out-link "$work/runner" "$repo#burrow-flake-check"
printf 'flake-check: built runner in %ss\n' "$((SECONDS - started))"
"$work/runner/bin/burrow-flake-check" "$1"
