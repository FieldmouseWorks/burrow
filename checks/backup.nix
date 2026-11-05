# Off-site backups of ZFS snapshots through restic and rclone, and the
# restore drill that checks them.
{ self, pkgs, ... }:
import ../tests/backup.nix { inherit self pkgs; }
