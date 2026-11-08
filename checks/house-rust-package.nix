# lib/project.nix's package of a Rust workspace: every member's binaries.
{ self, pkgs, system, ... }:
let
  pkg = (self.lib.project { self = ../tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
in pkgs.runCommand "house-rust-package-check" { } ''
  [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
  # What lib/project.nix's cargoTests must leave in: a program named tests,
  # and a member named tests that a glob finds (a module named tests that
  # isn't one, and a crate in tests/, the build itself needs).
  [ "$(${pkg}/bin/tests)" = tests ]
  [ "$(${pkg}/bin/house-fixture-tests-crate)" = crate ]
  touch $out
''
