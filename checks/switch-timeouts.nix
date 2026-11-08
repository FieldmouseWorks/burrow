{ self, pkgs, nixpkgs, ... }:
assert import ../tests/switch-timeouts.nix { inherit self pkgs nixpkgs; };
pkgs.runCommand "switch-timeouts-check" { } "touch $out"
