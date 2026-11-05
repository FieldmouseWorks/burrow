# lib/project.nix's package of a Rust workspace: every member's binaries.
{ self, pkgs, system, ... }:
let
  pkg = (self.lib.project { self = ../tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
in pkgs.runCommand "house-rust-package-check" { } ''
  [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
  touch $out
''
