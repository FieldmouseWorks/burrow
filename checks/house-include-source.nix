# lib/project.nix's `include`: the checks build from the repo's
# root with the workspace and only the files named outside it (its
# rust-toolchain.toml links to burrow's, outside it, so it dangles
# there, where nothing reads it). rust-test says it runs the root
# package's tests and the included file (lib/rust-test-files.nix), as
# paths from burrow's root where flake.nix re-exports it.
{ self, pkgs, system, houseInclude, ... }:
let
  runs = builtins.toJSON houseInclude.checks.${system}.rust-test.testFiles or null;
  reexported = builtins.toJSON self.checks.${system}.house-include-rust-test.testFiles or null;
in
pkgs.runCommand "house-include-source-check" { inherit runs reexported; } ''
  cd ${houseInclude.checks.${system}.rust-test.src}
  got=$(find . ! -type d | sort)
  want=$(printf '%s\n' ./spec/fact.txt ./ws/Cargo.lock ./ws/Cargo.toml ./ws/rust-toolchain.toml ./ws/src/lib.rs)
  [ "$got" = "$want" ] || { printf 'the source has:\n%s\n' "$got"; exit 1; }
  want='["ws/*","ws/src/**","ws/tests/**","ws/benches/**","ws/examples/**","spec/fact.txt","spec/fact.txt/**"]'
  [ "$runs" = "$want" ] || { printf 'rust-test says it runs %s, not %s\n' "$runs" "$want"; exit 1; }
  want='["tests/house/rust-include/ws/*","tests/house/rust-include/ws/src/**","tests/house/rust-include/ws/tests/**","tests/house/rust-include/ws/benches/**","tests/house/rust-include/ws/examples/**","tests/house/rust-include/spec/fact.txt","tests/house/rust-include/spec/fact.txt/**"]'
  [ "$reexported" = "$want" ] || { printf 'burrow re-exports it as running %s, not %s\n' "$reexported" "$want"; exit 1; }
  touch $out
''
