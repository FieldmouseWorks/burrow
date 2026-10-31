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
  # The board's own watchdog, by a name no other driver can take (and not
  # burrow-watchdog, which is the stall watchdog, watchdog.nix). The name
  # carries the identity: systemd reopens its watchdog only when the path it's
  # given changes, so a switch to another identity must change the path too.
  pinned = cfg.watchdog && cfg.watchdogIdentity != null;
  link = "hardware-watchdog-${lib.strings.sanitizeDerivationName cfg.watchdogIdentity}";
  device = "/dev/${link}";
  rule = ''SUBSYSTEM=="watchdog", ATTR{identity}=="${cfg.watchdogIdentity}", SYMLINK+="${link}"'';
  omit = { serviceConfig.ManagedOOMPreference = "omit"; };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/updates"; imports = [ (import ./updates.nix inputs) ]; }
  ];

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
    watchdogIdentity = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "[^\"]+");
      default = null;
      example = "iTCO_wdt";
      description = ''
        The identity of the board's own watchdog, the one that resets it, as
        /sys/class/watchdog/watchdog*/identity has it. systemd pets
        /dev/watchdog0, which is whichever watchdog driver came up first, and a
        kernel can bring a second one (a CPU's overclocking watchdog, say). With
        this set, systemd pets that device whatever the order, in the initrd
        too, and HOST-health's `watchdog` check fails unless it's the one armed.
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

    systemd.settings.Manager = lib.mkIf cfg.watchdog ({
      RuntimeWatchdogSec = "30s";
      RebootWatchdogSec = "10min";
    } // lib.optionalAttrs pinned {
      # systemd opens it once udev has made the link, if it's not there yet.
      WatchdogDevice = device;
    });
    # And in a systemd initrd, so a hang before the system is up resets too
    # (the driver has to be in boot.initrd.kernelModules for that).
    boot.initrd.systemd.settings.Manager = lib.mkIf (cfg.watchdog && config.boot.initrd.systemd.enable) ({
      RuntimeWatchdogSec = "30s";
    } // lib.optionalAttrs pinned { WatchdogDevice = device; });
    services.udev.extraRules = lib.mkIf pinned rule;
    boot.initrd.services.udev.rules = lib.mkIf (pinned && config.boot.initrd.systemd.enable) rule;
    # A switch that brings the rule only reloads udev's rules, and the link
    # would wait for the next boot: make it now. Bounded, well inside any
    # confirm window: activation waits for it while holding the lock the
    # switch's revert needs, so a stalled udev must fail it, not hang it.
    systemd.services.hardware-watchdog-link = lib.mkIf pinned {
      description = "Name the board's watchdog for systemd";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-udevd.service" ];
      restartTriggers = [ rule ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        TimeoutStartSec = "60s";
        ExecStart = [
          "${config.systemd.package}/bin/udevadm control --reload"
          "${config.systemd.package}/bin/udevadm trigger --subsystem-match=watchdog --action=change"
          "${config.systemd.package}/bin/udevadm settle --timeout=30"
        ];
      };
    };
    # The device systemd pets is the board's own, and armed. Found through
    # its major:minor, as systemd finds it, so any name for it works.
    burrow.updates.health.watchdog = lib.mkIf pinned ''
      dev=$(readlink -f "$(systemctl show -p WatchdogDevice --value)")
      [ -c "$dev" ] || { echo "systemd's watchdog device isn't there"; exit 1; }
      sys=/sys/dev/char/$(stat -c '%Hr:%Lr' "$dev")
      id=$(cat "$sys/identity") && state=$(cat "$sys/state") || exit 1
      [ "$id" = ${lib.escapeShellArg cfg.watchdogIdentity} ] || { echo "systemd pets $dev ($id), not ${cfg.watchdogIdentity}"; exit 1; }
      [ "$state" = active ] || { echo "$dev ($id) isn't armed"; exit 1; }
    '';

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
    systemd.services."sshd@" = lib.mkIf (cfg.oomd && config.services.openssh.enable) omit;
    systemd.services.sshd = lib.mkIf
      (cfg.oomd && config.services.openssh.enable && !config.services.openssh.startWhenNeeded) omit;
    systemd.services.tailscaled = lib.mkIf (cfg.oomd && config.services.tailscale.enable) omit;

    services.fwupd.enable = true;
    services.smartd.enable = true;

    burrow.claude.machineMd = lib.mkOrder 1400 ''
      ## Tools

      perf, bpftrace and the bcc tools, strace, gdb, tcpdump, nvme-cli, smartctl,
      dmidecode, sensors, fwupdmgr. Most need sudo.
    '';
  };
}
