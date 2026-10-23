# The snapshot side of "instant undo": the Claude guard (claude.nix) takes an
# `@pre-*` snapshot of burrow.undo.datasets before anything deletes or rewrites
# files. This lets the user take them without sudo, and prunes them.
#
# Rolling snapshots (sanoid or similar) are yours to set up; they're what makes
# undo reach back further than the last guarded command.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.undo;
  zfs = config.boot.zfs.package;
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.undo = {
    enable = lib.mkEnableOption "guard snapshots of burrow.undo.datasets, delegated and pruned";
    keepDays = lib.mkOption {
      type = lib.types.ints.positive;
      default = 3;
      description = "How long a guard snapshot is kept.";
    };
    notes = lib.mkOption {
      type = lib.types.str;
      default = "";
      example = "Sanoid also snapshots rpool/safe every 15 minutes.";
      description = "Said after the guard snapshots in the machine CLAUDE.md's Undo section.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.prune-pre-snapshots = {
      description = "Drop guard-hook snapshots older than ${toString cfg.keepDays} days";
      serviceConfig.Type = "oneshot";
      path = [ zfs pkgs.gawk pkgs.coreutils ];
      script = ''
        cutoff=$(( $(date +%s) - ${toString cfg.keepDays} * 86400 ))
        zfs list -H -p -t snapshot -o name,creation -r ${lib.escapeShellArgs cfg.datasets} \
          | awk -v c="$cutoff" '$1 ~ /@pre-/ && $2 < c { print $1 }' \
          | while read -r snap; do zfs destroy "$snap"; done
      '';
    };
    systemd.timers.prune-pre-snapshots = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = "hourly";
        Persistent = true;
      };
    };

    # Let the user snapshot (but not destroy) these datasets without sudo, so
    # the guard hook never needs a password or a prompt.
    systemd.services.zfs-delegate-dev = {
      description = "Delegate ZFS snapshot rights to ${config.burrow.user}";
      wantedBy = [ "multi-user.target" ];
      after = [ "zfs-mount.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      path = [ zfs ];
      script = lib.concatMapStrings (ds: ''
        zfs allow -u ${config.burrow.user} snapshot,mount ${ds}
      '') cfg.datasets;
    };

    burrow.claude.machineMd = lib.mkOrder 1100 ''
      ## Undo

      - Files: anything that deletes or rewrites files in
        ${lib.concatStringsSep " or " cfg.datasets} gets a ZFS snapshot first
        (`zfs list -t snapshot | grep @pre-`), kept ${toString cfg.keepDays} days.${lib.optionalString (cfg.notes != "") "\n  ${cfg.notes}"}
        Restore by copying out of the dataset's `.zfs/snapshot/<name>/`.
      - System: generations, as above.
    '';
  };
}
