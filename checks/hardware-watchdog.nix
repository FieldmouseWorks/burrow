# systemd pets the board's own watchdog, whichever came up first.
{ self, pkgs, ... }:
import ../tests/hardware-watchdog.nix { inherit self pkgs; }
