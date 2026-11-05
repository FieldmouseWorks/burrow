# Stale Remote Control servers catch up once idle, with a stand-in for Claude.
{ self, pkgs, ... }:
import ../tests/remote-control.nix { inherit self pkgs; }
