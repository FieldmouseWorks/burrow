# HOST-update's time limit, which its waits for a heavy slot don't use up
# and which a daemon-reload doesn't reset, on real systemd.
{ self, pkgs, ... }:
import ../tests/update-limit.nix { inherit self pkgs; }
