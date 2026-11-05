# A session past its server's memory cap dies alone, with a stand-in for Claude.
{ self, pkgs, ... }:
import ../tests/remote-control-memory.nix { inherit self pkgs; }
