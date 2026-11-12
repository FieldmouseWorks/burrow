{ self, pkgs, ... }:
import ../tests/settings-off-switch.nix { inherit self pkgs; }
