{ self, pkgs, ... }:
import ../tests/settings-vacate.nix { inherit self pkgs; }
