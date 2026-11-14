# HOST-switch never waits behind a run that was going before it.
{ self, pkgs, ... }:
import ../tests/switch-queue.nix { inherit self pkgs; }
