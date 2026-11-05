# An App's tokens: who gets them, and git and gh acting as it.
{ self, pkgs, ... }:
import ../tests/github.nix { inherit self pkgs; }
