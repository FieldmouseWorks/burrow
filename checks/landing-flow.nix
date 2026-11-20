# conaryd runs only while burrow-landing gives it a repo (tests/landing.nix).
{ self, pkgs, ... }:
import ../tests/landing.nix { inherit self pkgs; }
