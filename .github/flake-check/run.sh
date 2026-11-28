# Only bootstrap glue. The policy lives in crates/burrow-flake-check.
# Compile the workflow's pinned sources without Cargo or registry work. Never
# reuse an executable a preceding step could have left in RUNNER_TEMP.
set -eo pipefail
here=$(cd -- "${BASH_SOURCE[0]%/*}" && pwd)
work=$(mktemp -d "$RUNNER_TEMP/runner.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
# Keep the compiler rooted.
compiler_started=$SECONDS
build_compiler() {
  nix-build --extra-experimental-features 'nix-command flakes' "$here/compiler.nix" --out-link "$work/compiler"
}
if ! compiler=$(build_compiler 2>"$work/compiler.err"); then
  cat "$work/compiler.err" >&2
  grep -qE 'is required, but there is no substituter that can build it|some substitutes for the outputs of derivation .* failed|disabling binary cache ' "$work/compiler.err" || exit 1
  url=${BUILDS_SUBSTITUTER:-}; url=${url%%\?*}; url=${url%/}
  if [ -n "$url" ] && URL="$url" awk '
    BEGIN { u = ENVIRON["URL"]; q = sprintf("%c", 39); u1 = q u q; u2 = q u "/" q }
    index($0, "copying path " q) == 1 && (index($0, " from " u1) || index($0, " from " u2)) && split($0, p, q) >= 4 { c[p[2]] = 1 }
    index($0, "error: path " q) == 1 && index($0, " is required, but there is no substituter that can build it") && split($0, p, q) >= 2 && c[p[2]] { f = 1; exit 0 }
    index($0, "disabling binary cache " u1) || index($0, "disabling binary cache " u2) ||
      index($0, "substituter " u1 " is ") || index($0, "substituter " u2 " is ") ||
      index($0, "unable to download " q u "/") || index($0, "unable to download " u1) ||
      (index($0, "NAR for ") && (index($0, "fetched from " u1 " is incomplete") || index($0, "fetched from " u2 " is incomplete"))) { f = 1; exit 0 }
    END { exit !f }
  ' "$work/compiler.err"; then
    rest=()
    for s in $(nix config show substituters 2>/dev/null || true); do
      u=${s%%\?*}; u=${u%/}
      [ "$u" = "$url" ] || rest+=("$s")
    done
    echo "substituter '$url' failed during substitution; dropping it and retrying without it" >&2
    export NIX_CONFIG="${NIX_CONFIG:+$NIX_CONFIG$'\n'}substituters = ${rest[*]}"$'\n'"extra-substituters ="
    export BUILDS_DROPPED=1
  else
    sleep 3
  fi
  compiler=$(build_compiler)
else
  cat "$work/compiler.err" >&2
fi
printf 'flake-check: provisioned compiler in %ss\n' "$((SECONDS - compiler_started))"
compile_started=$SECONDS
# Rustup must not read a caller's rust-toolchain.toml. The edition is the
# workspace's (crates/Cargo.toml), so Cargo, Clippy and this agree on the source.
(cd -- "$work" && PATH="$compiler/bin:$PATH" "$compiler/bin/rustc" --edition=2024 "$here/../../crates/burrow-flake-check/src/main.rs" -o runner)
printf 'flake-check: compiled runner in %ss\n' "$((SECONDS - compile_started))"
"$work/runner" "$1"
