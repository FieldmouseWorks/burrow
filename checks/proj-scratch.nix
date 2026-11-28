# A finished job's scratch goes, by its root-side unit, whatever is in it.
{ self, pkgs, ... }:
import ../tests/proj-scratch.nix { inherit self pkgs; }
