# HOST-health's ZFS check: the pools the machine needs, by name or
# GUID, with a stand-in zpool.
{ self, pkgs, ... }:
import ../tests/health-zfs.nix { inherit self pkgs; }
