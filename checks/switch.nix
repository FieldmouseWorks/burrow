# HOST-switch's revert when the window runs out mid-activation.
{ self, pkgs, ... }:
import ../tests/switch.nix { inherit self pkgs; }
