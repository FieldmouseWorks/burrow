# systemd-oomd leaves a Remote Control server whole, with a stand-in for Claude.
{ self, pkgs, ... }:
import ../tests/remote-control-oomd.nix { inherit self pkgs; }
