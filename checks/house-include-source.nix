# lib/project.nix's `include`: the checks build from the repo's
# root with the workspace and only the files named outside it (its
# rust-toolchain.toml links to burrow's, outside it, so it dangles
# there, where nothing reads it).
{ pkgs, system, houseInclude, ... }:
pkgs.runCommand "house-include-source-check" { } ''
  cd ${houseInclude.checks.${system}.rust-test.src}
  got=$(find . ! -type d | sort)
  want=$(printf '%s\n' ./spec/fact.txt ./ws/Cargo.lock ./ws/Cargo.toml ./ws/rust-toolchain.toml ./ws/src/lib.rs)
  [ "$got" = "$want" ] || { printf 'the source has:\n%s\n' "$got"; exit 1; }
  touch $out
''
