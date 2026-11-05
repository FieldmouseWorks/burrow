{ self, pkgs, ... }:
import ../tests/proj-slots.nix { inherit self pkgs; }
