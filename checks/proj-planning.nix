{ self, pkgs, ... }:
import ../tests/proj-planning.nix { inherit self pkgs; }
