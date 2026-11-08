# Only bootstrap glue. The policy lives in crates/burrow-flake-check.
# Compile the workflow's pinned sources without Cargo or registry work. Never
# reuse an executable a preceding step could have left in RUNNER_TEMP.
set -eo pipefail
here=$(cd -- "${BASH_SOURCE[0]%/*}" && pwd)
work=$(mktemp -d "$RUNNER_TEMP/runner.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Rustup must not read a caller's rust-toolchain.toml.
(cd -- "$work" && rustc --edition=2021 "$here/../../crates/burrow-flake-check/src/main.rs" -o runner)
"$work/runner" "$1"
