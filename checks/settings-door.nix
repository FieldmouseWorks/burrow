{ self, pkgs, ... }:
import ../tests/settings-door.nix { inherit self pkgs; }
