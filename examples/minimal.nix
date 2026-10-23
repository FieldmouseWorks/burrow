# The smallest machine with every burrow module on: what the `example` check
# builds. A real one adds its disks, network, boot loader and logins.
{ lib, ... }:
{
  networking.hostName = "burrow";
  networking.hostId = "00000000";
  system.stateVersion = "26.05";
  nixpkgs.hostPlatform = "x86_64-linux";
  time.timeZone = "UTC";

  boot.loader.grub.devices = [ "nodev" ];
  fileSystems."/" = {
    device = "rpool/root";
    fsType = "zfs";
  };
  boot.supportedFilesystems = [ "zfs" ];
  boot.zfs.forceImportRoot = false;

  users.users.dev = {
    isNormalUser = true;
    uid = 1001;
    group = "dev";
    extraGroups = [ "wheel" "kvm" ];
  };
  users.groups.dev.gid = 1001;
  security.sudo.wheelNeedsPassword = false;

  burrow = {
    owner = "Sam";
    undo.datasets = [ "rpool/safe/home" "rpool/safe/data" ];
    claude.enable = true;
    undo.enable = true;
    switch.enable = true;
    proj.enable = true;
    remoteControl.enable = true;
    agents.enable = true;
    toolbox.enable = true;
    ops.enable = true;
    robot = {
      enable = true;
      serverIp = "203.0.113.7";
    };
  };
}
