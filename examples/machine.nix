# A bare machine for the checks: just enough NixOS (and a user with a fixed
# uid) for burrow's modules to build on.
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
}
