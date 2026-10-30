# Kernel, hardware and firmware tooling for an agent that runs the machine
# all the way down (run it with sudo), plus a systemd watchdog so a hung box
# reboots into its last good generation, and systemd-oomd so a box running
# out of memory loses the job that ate it rather than drowning. Firmware
# updates are on offer; installing one is an R2 (claude.nix).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.ops;
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
        memory for 30s. That's every system slice but the root and the users'
        (a login session is watched on its own), and in each user's manager
        its app, background and session slices. The kernel's own OOM killer
        waits until memory and swap are gone, and
        with zram it can thrash first for as long as it likes. sshd
        (socket-activated or not), tailscaled, the switch's own units
        (burrow.switch) and Remote Control servers (burrow.remoteControl) are
        never picked. claude.slice, with proj's jobs and the servers, is the
        kernel's: it and each unit in it have a cap of their own and no swap.
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
    # a watch on the jobs would kill one whole for it. Swap is watched on the
    # root: claude.slice holds none, so that watch never picks a Claude.
    systemd.oomd = lib.mkIf cfg.oomd {
      enable = true;
      settings.OOM.DefaultMemoryPressureLimit = "80%";
    };
    # Every system slice, but where a slice's own drop-in has the same name
    # (overrides.conf, for all of NixOS's), the slice's wins outright.
    systemd.units.slice = lib.mkIf cfg.oomd {
      text = ''
        [Slice]
        ManagedOOMMemoryPressure=kill
      '';
      overrideStrategy = "asDropin";
    };
    systemd.slices = lib.mkIf cfg.oomd {
      "-" = {
        overrideStrategy = "asDropin";
        sliceConfig = {
          ManagedOOMSwap = "kill";
          ManagedOOMMemoryPressure = "auto";
        };
      };
      user = {
        overrideStrategy = "asDropin";
        sliceConfig.ManagedOOMMemoryPressure = "auto";
      };
      "user-" = {
        overrideStrategy = "asDropin";
        sliceConfig.ManagedOOMMemoryPressure = "auto";
      };
    };
    # Each login session (logind's session-N.scope, in user-UID.slice).
    systemd.units."session-.scope" = lib.mkIf cfg.oomd {
      text = ''
        [Scope]
        ManagedOOMMemoryPressure=kill
      '';
      overrideStrategy = "asDropin";
    };
    # Everything else a user runs is in these.
    systemd.user.slices = lib.mkIf cfg.oomd (lib.genAttrs [ "app" "background" "session" ] (_: {
      overrideStrategy = "asDropin";
      sliceConfig.ManagedOOMMemoryPressure = "kill";
    }));
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
