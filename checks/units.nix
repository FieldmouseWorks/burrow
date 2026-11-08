# Failed units and core dumps reach the stall watchdog (tests/units.nix).
{ self, pkgs, ... }:
import ../tests/units.nix { inherit self pkgs; }
