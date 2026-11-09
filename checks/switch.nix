# HOST-switch's revert when the window runs out mid-activation.
{ self, pkgs, ... }:
assert import ../tests/locked-repo.nix { inherit (pkgs) lib; };
assert import ../tests/running-inputs.nix { inherit self pkgs; };
import ../tests/switch.nix { inherit self pkgs; }
