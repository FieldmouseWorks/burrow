{ self, pkgs, ... }:
import ../tests/proj-github.nix { inherit self pkgs; }
