# lib/project.nix's package of a Rust workspace: every member's binaries.
{ self, pkgs, system, ... }:
let
  pkg = (self.lib.project { self = ../tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
in pkgs.runCommand "house-rust-package-check" { } ''
  # Its answer comes from modules named like tests that the library uses:
  # one declared plainly, one by a path attribute that a #[cfg(test)]
  # module's path attribute names too.
  [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
  # What lib/project.nix's cargoTests must leave in: a program named tests,
  # and a member named tests that a glob finds (a module named tests that
  # isn't one, and a crate in tests/, the build itself needs).
  [ "$(${pkg}/bin/tests)" = tests ]
  [ "$(${pkg}/bin/main)" = main ]
  [ "$(${pkg}/bin/house-fixture-tests-crate)" = crate ]
  # And a package's tests whose role in its build can't be told: a build
  # script in tests/, a program that includes a file from it by a path it
  # makes, one whose module is there by a conditional path attribute (and
  # a test target declared at a module's file, which the build itself
  # needs).
  [ "$(${pkg}/bin/house-fixture-built)" = built ]
  [ "$(${pkg}/bin/house-fixture-included)" = 9 ]
  [ "$(${pkg}/bin/house-fixture-pathed)" = 5 ]
  touch $out
''
