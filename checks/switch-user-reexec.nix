# HOST-switch leaves a user manager running unless systemd itself changed.
{ self, pkgs, ... }:
import ../tests/switch-user-reexec.nix { inherit self pkgs; }
