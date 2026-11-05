{ self, pkgs, ... }:
import ../tests/proj-second-runs.nix { inherit self pkgs; }
