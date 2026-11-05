# A turn's clock, which stands still while it waits in line.
{ self, pkgs, ... }:
import ../tests/proj-clock.nix { inherit self pkgs; }
