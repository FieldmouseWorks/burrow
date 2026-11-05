# systemd-oomd kills what eats the memory before the machine drowns.
{ self, pkgs, ... }:
import ../tests/oomd.nix { inherit self pkgs; }
