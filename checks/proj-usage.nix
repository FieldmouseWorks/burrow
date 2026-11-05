# A run refused for usage holds the line till its limit resets.
{ self, pkgs, ... }:
import ../tests/proj-usage.nix { inherit self pkgs; }
