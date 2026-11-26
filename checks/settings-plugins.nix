{ self, pkgs, ... }:
import ../tests/settings-plugins.nix { inherit self pkgs; }
