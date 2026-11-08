# Only bootstrap glue. The policy lives in crates/burrow-flake-check.
# Compile the workflow's pinned sources without Cargo or registry work. Never
# reuse an executable a preceding step could have left in RUNNER_TEMP.
set -eo pipefail
here=$(cd -- "${BASH_SOURCE[0]%/*}" && pwd)
work=$(mktemp -d "$RUNNER_TEMP/runner.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Keep the compiler rooted, and do not consult an unrestored kept-build cache.
compiler_started=$SECONDS
compiler=$(nix-build --extra-experimental-features 'nix-command flakes' "$here/compiler.nix" --out-link "$work/compiler" --option extra-substituters "")
printf 'flake-check: provisioned compiler in %ss\n' "$((SECONDS - compiler_started))"
compile_started=$SECONDS
# Rustup must not read a caller's rust-toolchain.toml.
(cd -- "$work" && PATH="$compiler/bin:$PATH" "$compiler/bin/rustc" --edition=2021 "$here/../../crates/burrow-flake-check/src/main.rs" -o runner)
printf 'flake-check: compiled runner in %ss\n' "$((SECONDS - compile_started))"
"$work/runner" "$1"
