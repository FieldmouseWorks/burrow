# Off-site backups: HOST-backup sends each new snapshot of the datasets worth
# keeping to a restic repository through rclone (Google Drive, S3, another box
# over SFTP: anything rclone reaches). Restic deduplicates and compresses
# (zstd), so after the first run each one sends only what changed, and
# encrypts before anything leaves, with a password the box and its owner hold.
#
# The snapshots are yours (sanoid or similar, as for undo); a backup pass runs
# when the units in `after` finish, so each new snapshot is off the box
# minutes after it's taken, and once a day regardless, in case they stop.
# Once a day it also forgets what `keep` doesn't keep, reads a share of the
# repository back, checks the newest backup holds everything its snapshot
# had, and restores a few paths to compare byte for byte (backup/backup.sh).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.backup;
  inherit (config.burrow) prefix user tools;
  account = config.users.users.${user};
  name = "${prefix}-backup";

  vars = {
    NAME = name;
    HOST = config.networking.hostName;
    DIR = "${config.burrow.stateDir}/backup";
    REPO = cfg.repository;
    PASSWORD = cfg.passwordFile;
    RCLONE_CONFIG_FILE = if cfg.rcloneConfigFile == null then "" else cfg.rcloneConfigFile;
    PATTERN = cfg.snapshots;
    EXCLUDE_FILE = pkgs.writeText "${name}-exclude" (lib.concatLines cfg.exclude);
    VERIFY_DAYS = toString cfg.verifyDays;
    PACK_SIZE = toString cfg.packSize;
    STALE_AFTER = toString cfg.staleAfter;
    SCRATCH = config.burrow.scratchDir;
    PROJ = lib.optionalString (tools ? proj) "${tools.proj}/bin/proj";
    PROJECT = baseNameOf config.burrow.flake;
    OWNER = config.burrow.owner;
    NOTIFY = "${tools.notify}/bin/${prefix}-notify";
    BURROW_USER = user;
    HOME_DIR = account.home;
    USER_UID = toString account.uid;
  };

  # Lists, as bash arrays: a path with a space in it stays one path.
  lists = {
    DATASETS = cfg.datasets;
    DRILL = cfg.drill;
    KEEP = lib.concatLists (lib.mapAttrsToList (k: n: [ "--keep-${k}" (toString n) ]) cfg.keep);
  };

  backup = pkgs.writeShellApplication {
    inherit name;
    runtimeInputs = with pkgs; [ restic rclone jq coreutils findutils diffutils gnugrep gawk util-linux bash config.boot.zfs.package ];
    text = lib.concatStrings (lib.mapAttrsToList (n: v: "${n}=${lib.escapeShellArg v}\n") vars)
      + lib.concatStrings (lib.mapAttrsToList (n: v: "${n}=(${lib.escapeShellArgs v})\n") lists)
      + builtins.readFile ./backup/backup.sh;
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/notify"; imports = [ (import ./notify.nix inputs) ]; }
  ];

  options.burrow.backup = with lib; {
    enable = mkEnableOption "HOST-backup, each new snapshot of burrow.backup.datasets off the box";

    repository = mkOption {
      type = types.str;
      example = "rclone:gdrive:backups/mybox";
      description = ''
        The restic repository, usually `rclone:REMOTE:PATH` for one of
        rclone's remotes. Created on the first run.
      '';
    };

    passwordFile = mkOption {
      type = types.str;
      example = "/run/secrets/backup-password";
      description = ''
        The repository's password, readable by root (a sops-nix secret, say).
        It's the key to everything in the repository, and the box holds the
        only copy: keep another one somewhere that doesn't go down with it.
      '';
    };

    rcloneConfigFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/backup-rclone";
      description = ''
        An rclone config with the remote the repository names. Each run works
        on a copy, so the refreshed tokens rclone writes back never touch it.
      '';
    };

    datasets = mkOption {
      type = types.listOf types.str;
      example = [ "rpool/safe/persist" "rpool/safe/home" "rpool/safe/data" ];
      description = ''
        The ZFS datasets to back up, each at the path it's mounted on. Child
        datasets aren't included: list them too. None may be mounted at /, or
        over /nix/store or /run, which the backup runs from.
      '';
    };

    snapshots = mkOption {
      type = types.str;
      default = "autosnap_*_hourly";
      description = ''
        Which of the datasets' snapshots get backed up, as a glob on the
        snapshot's name; the default is sanoid's hourly ones. Every dataset
        needs one of the same name. Each pass takes the newest one there is
        and holds it with the ZFS hold `burrow-backup` until the next is
        off the box.
      '';
    };

    after = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "sanoid.service" ];
      description = "Units whose every run is followed by a backup pass: whatever takes the snapshots.";
    };

    backstop = mkOption {
      type = types.str;
      default = "daily";
      description = ''
        When a pass runs regardless (systemd's OnCalendar), so a box whose
        snapshots stopped still hears about it.
      '';
    };

    exclude = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "/data/scratch" "/home/*/.cache" "node_modules" ];
      description = ''
        restic exclude patterns, as the paths are on the box. Folders with a
        CACHEDIR.TAG (cargo's target, for one) or a `.nobackup` file are left
        out anyway.
      '';
    };

    keep = mkOption {
      type = types.attrsOf types.ints.positive;
      default = {
        hourly = 48;
        daily = 30;
        weekly = 12;
        monthly = 24;
        yearly = 10;
      };
      description = "How many backups to keep, as restic's `--keep-NAME N`.";
    };

    verifyDays = mkOption {
      type = types.ints.positive;
      default = 30;
      description = ''
        Each day reads 1/verifyDays of the repository back and checks it, so
        all of it is read back over that many days.
      '';
    };

    drill = mkOption {
      type = types.listOf types.str;
      default = [ config.burrow.flake ];
      defaultText = literalExpression "[ config.burrow.flake ]";
      description = ''
        What the daily drill restores from the newest backup and compares
        with the snapshot it was read from, entry for entry and byte for
        byte: directories, files or symlinks. Before that, the drill checks
        the backup holds everything the snapshot had (what the backup's own
        rules leave out aside) by backing the snapshot up again and comparing
        the two listings: a backup pass, forgotten after, not a restore.
        `HOST-backup drill PATH` drills anything else.
      '';
    };

    staleAfter = mkOption {
      type = types.ints.positive;
      default = 6;
      description = ''
        Hours the newest copy off the box may age before the owner hears
        about it. Failures before that go to a Claude job, with proj.
      '';
    };

    packSize = mkOption {
      type = types.ints.positive;
      default = 64;
      description = "restic's pack size in MiB: bigger means fewer files on the remote.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.datasets != [ ];
        message = "burrow.backup: set burrow.backup.datasets to the datasets to back up.";
      }
    ];

    environment.systemPackages = [ backup ];

    systemd.services.${name} = {
      description = "Back up the newest snapshot of ${lib.concatStringsSep ", " cfg.datasets} off the box";
      wantedBy = cfg.after;
      after = cfg.after ++ [ "network-online.target" "zfs-mount.service" ];
      wants = [ "network-online.target" ];
      # A switch never starts it: only what takes snapshots and the backstop do.
      restartIfChanged = false;
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${backup}/bin/${name}";
        Nice = 10;
        IOSchedulingClass = "idle";
        CPUSchedulingPolicy = "batch";
      };
    };
    systemd.timers.${name} = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.backstop;
        Persistent = true;
        RandomizedDelaySec = "30m";
      };
    };

    burrow.claude.machineMd = lib.mkOrder 1150 ''
      ## Off-site backups

      Each new snapshot (`${cfg.snapshots}`) of ${lib.concatStringsSep ", " cfg.datasets}
      goes to restic at `${cfg.repository}` within minutes (`${name}`, after
      ${if cfg.after == [ ] then "the daily backstop" else lib.concatStringsSep ", " cfg.after}).
      `${name} status` says where it stands; `${name} drill PATH` restores
      PATH from the newest backup to scratch and checks it against its
      snapshot; `${name} restic ...` is restic on that repository (restore
      with `restic restore latest --target DIR --include PATH`). Never forget,
      prune or delete in it by hand: it's the only copy off the box.
    '';
  };
}
