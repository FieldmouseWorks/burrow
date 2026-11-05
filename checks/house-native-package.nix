# lib/project.nix's package of a workspace that links a native
# library: the build found the one the project names.
{ self, pkgs, system, houseNative, ... }:
let
  pkg = (self.lib.project { self = ../tests/house/rust-native; systems = [ system ]; rust = houseNative; }).packages.${system}.default;
in pkgs.runCommand "house-native-package-check" { } ''
  [ "$(${pkg}/bin/house-native)" = ${pkgs.zlib.version} ]
  touch $out
''
