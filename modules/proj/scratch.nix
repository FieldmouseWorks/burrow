# The one owner of removing a finished job's scratch (crates/burrow-scratch).
# proj sets SCRATCH/jobs/ID aside under a name starting .gone- and takes its
# worktrees off their repos (burrow-proj's scratch.rs); a path unit starts the
# sweep, as root, whenever such a name exists, so what the job's own user
# can't remove (a root-owned chroot, another service user's files, an
# interrupted removal) still goes. The sweep is confined to SCRATCH/jobs and
# descends by descriptor, through no link or mount. An entry it can't remove
# is renamed .stuck-..., out of the watched name, and tried again each later
# sweep; a unit of its own after each sweep gives each one notice (HOST-notify),
# closed once it's gone, so raising it never widens what the sweep can write.
inputs:
{ config, lib, pkgs, ... }:
let
  inherit (config.burrow) stateDir;
  jobs = "${config.burrow.scratchDir}/jobs";
  sweeper = "${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-scratch}/bin/burrow-scratch";
  # The prefix proj sets a job's scratch aside under, read from the one line
  # that spells it, so the glob can't drift from it: evaluation fails if the
  # line isn't there.
  spelled = builtins.match ''.*pub const GONE: &str = "([^"]+)";.*''
    (builtins.readFile ../../crates/burrow-scratch/src/lib.rs);
  gone = if spelled == null
    then throw "crates/burrow-scratch/src/lib.rs no longer spells GONE as this module reads it"
    else builtins.head spelled;
  records = "${stateDir}/proj-scratch-stuck";
  notify = lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify);
in
{
  config = lib.mkIf config.burrow.proj.enable {
    systemd.paths.proj-scratch-sweep = {
      description = "A finished job's scratch, set aside to be removed";
      wantedBy = [ "paths.target" ];
      pathConfig.PathExistsGlob = "${jobs}/${gone}*";
    };

    systemd.services.proj-scratch-sweep = {
      description = "Remove the scratch proj set aside once a job was over";
      # Started once for each job's end, however many end at once: a start
      # limit would fail the path unit, and then nothing is removed again.
      startLimitIntervalSec = 0;
      # The stuck entries' notices, after every sweep, however it went.
      wants = [ "proj-scratch-stuck.service" ];
      before = [ "proj-scratch-stuck.service" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${sweeper} sweep ${jobs}";
        # Root, with only what unlinking another user's files in another
        # user's dirs takes, writing nowhere but SCRATCH/jobs.
        CapabilityBoundingSet = [ "CAP_DAC_OVERRIDE" "CAP_FOWNER" ];
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ReadWritePaths = [ jobs ];
        ProtectHome = "read-only";
        PrivateTmp = true;
        PrivateDevices = true;
        PrivateNetwork = true;
        PrivateIPC = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        RestrictAddressFamilies = "none";
        RestrictNamespaces = true;
        RestrictRealtime = true;
        # Not RestrictSUIDSGID: systemd can't filter openat2's flags, so it
        # refuses openat2 outright (ENOSYS), and the sweep descends by it. It
        # makes no file, and holds no CAP_FSETID or CAP_CHOWN to set one's bits.
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged @resources" ];
        # A descriptor a level, to burrow-scratch's DEPTH and past it.
        LimitNOFILE = 8192;
        # Gigabytes of a build's output: behind everything else on the disks.
        Nice = 19;
        IOSchedulingClass = "idle";
      };
    };

    systemd.services.proj-scratch-stuck = {
      description = "The owner's notice for each finished job's scratch the sweep couldn't remove";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${sweeper} notice ${jobs} ${records} proj-scratch-sweep.service ${notify}";
      };
    };

    systemd.tmpfiles.rules = [
      "d ${records} 0700 root root -"
    ];
  };
}
