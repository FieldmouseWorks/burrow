# Kernel, hardware and firmware tooling for an agent that runs the machine
# all the way down (run it with sudo), plus a systemd watchdog so a hung box
# reboots into its last good generation, and systemd-oomd so a box running
# out of memory loses the job that ate it rather than drowning. Firmware
# updates are on offer; installing one is an R2 (claude.nix).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.ops;
  # One of oomd's pressure watches, or a unit's way out of it, as a drop-in
  # with a name of its own: a unit's own one stands in for the one for every
  # slice, and NixOS's (overrides.conf, for any setting) stands in for
  # neither.
  pressure = manager: unit: action: pkgs.writeTextDir "lib/systemd/${manager}/${unit}.d/50-burrow-oomd.conf" ''
    [${if lib.hasSuffix ".scope" unit then "Scope" else "Slice"}]
    ManagedOOMMemoryPressure=${action}
  '';
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.ops = {
    enable = lib.mkEnableOption "kernel, hardware and firmware tools, fwupd, smartd";
    watchdog = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Have systemd pet the hardware watchdog: if PID 1 stops answering for
        30s the board resets itself, in the initrd too. Load your board's
        watchdog driver (boot.kernelModules, and boot.initrd.kernelModules) for
        it to do anything.
      '';
    };
    oomd = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Have systemd-oomd kill the unit that is drowning the machine:
        whichever holds the most swap once swap is 90% full, and whichever
        reclaims the most once a slice spends 80% of its time stalled on
        memory for 30s: every slice but the root and the users' (a login
        session is watched on its own), and in each user's manager every
        slice but its root and claude.slice. The kernel's own OOM killer
        waits until memory and swap are gone, and with zram it can thrash
        first for as long as it likes. sshd (socket-activated or not),
        tailscaled, the switch's own units (burrow.switch) and Remote Control
        servers (burrow.remoteControl) are never picked. claude.slice, with
        proj's jobs and the servers, is the kernel's: it and each unit in it
        have a cap of their own and no swap.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = with pkgs; [
      # processes and kernel
      strace
      ltrace
      gdb
      perf
      bpftrace
      sysstat
      iotop
      # network
      tcpdump
      ethtool
      mtr
      dnsutils
      # hardware and firmware
      nvme-cli
      smartmontools
      dmidecode
      usbutils
      lm_sensors
      efibootmgr
      flashrom
    ];

    programs.bcc.enable = true;

    systemd.settings.Manager = lib.mkIf cfg.watchdog {
      RuntimeWatchdogSec = "30s";
      RebootWatchdogSec = "10min";
    };
    # And in a systemd initrd, so a hang before the system is up resets too
    # (the driver has to be in boot.initrd.kernelModules for that).
    boot.initrd.systemd.settings.Manager.RuntimeWatchdogSec =
      lib.mkIf (cfg.watchdog && config.boot.initrd.systemd.enable) "30s";

    # Pressure is watched on every slice but the ones that hold Remote Control
    # servers: the root, user.slice, user-UID.slice, and in a user's manager
    # its root and claude.slice. oomd honours a user's unit's
    # ManagedOOMPreference only for a watch on a cgroup that user owns, so a
    # watch on any of the first three would kill a whole server, every
    # session on it, and one in the user's manager would kill a bystander for
    # pressure inside a server. Nor inside claude.slice: at its MemoryHigh
    # every job under it is slowed, which is what that soft limit is for, and
    # a watch on the jobs would kill one whole for it. A login session, in
    # user-UID.slice, is watched on its own. Swap is watched on the root:
    # claude.slice holds none, so that watch never picks a Claude.
    systemd.oomd = lib.mkIf cfg.oomd {
      enable = true;
      settings.OOM.DefaultMemoryPressureLimit = "80%";
    };
    systemd.slices."-".sliceConfig.ManagedOOMSwap = lib.mkIf cfg.oomd "kill";
    systemd.packages = lib.mkIf cfg.oomd ([
      (pressure "system" "slice" "kill")
      (pressure "system" "session-.scope" "kill")
      (pressure "user" "slice" "kill")
    ] ++ map (unit: pressure "system" unit "auto") [ "-.slice" "user.slice" "user-.slice" ]
      # claude-.slice: every slice in claude.slice, proj's and the servers'.
      ++ map (unit: pressure "user" unit "auto") [ "-.slice" "claude.slice" "claude-.slice" ]);
    # The ways in. openssh always has sshd@ (each connection's sshd, with
    # startWhenNeeded) and has sshd only without it; naming a unit it doesn't
    # have would make a stub of it.
    systemd.services = lib.mkIf cfg.oomd (lib.genAttrs
      (lib.optionals config.services.openssh.enable
        ([ "sshd@" ] ++ lib.optional (!config.services.openssh.startWhenNeeded) "sshd")
        ++ lib.optional config.services.tailscale.enable "tailscaled")
      (_: { serviceConfig.ManagedOOMPreference = "omit"; }));

    services.fwupd.enable = true;
    services.smartd.enable = true;

    burrow.claude.machineMd = lib.mkOrder 1400 ''
      ## Tools

      perf, bpftrace and the bcc tools, strace, gdb, tcpdump, nvme-cli, smartctl,
      dmidecode, sensors, fwupdmgr. Most need sudo.
    '';
  };
}
