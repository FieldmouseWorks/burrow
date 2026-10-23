# Kernel, hardware and firmware tooling for an agent that runs the machine
# all the way down (run it with sudo), plus a systemd watchdog so a hung box
# reboots into its last good generation. Firmware updates are on offer;
# installing one is an R2 (claude.nix).
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

    services.fwupd.enable = true;
    services.smartd.enable = true;

    burrow.claude.machineMd = lib.mkOrder 1400 ''
      ## Tools

      perf, bpftrace and the bcc tools, strace, gdb, tcpdump, nvme-cli, smartctl,
      dmidecode, sensors, fwupdmgr. Most need sudo.
    '';
  };
}
