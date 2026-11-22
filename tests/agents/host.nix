# The host the sandboxes' VM tests (../agents.nix, ../agents-plugins.nix)
# share: the user, and a pool laid out like burrow.agents' defaults on the
# spare disk.
{ pkgs, ... }:
{
  users.users.dev = {
    isNormalUser = true;
    uid = 1001;
    group = "dev";
    extraGroups = [ "wheel" "kvm" ];
  };
  users.groups.dev.gid = 1001;
  security.sudo.wheelNeedsPassword = false;

  virtualisation = {
    memorySize = 6144;
    cores = 4;
    emptyDiskImages = [ 16384 ];
  };

  boot.supportedFilesystems = [ "zfs" ];
  boot.zfs.forceImportRoot = false;
  networking.hostId = "8425e349";

  # rpool on the spare disk, with the default data dataset.
  systemd.services.make-rpool = {
    wantedBy = [ "multi-user.target" ];
    before = [ "agents-dataset.service" ];
    requiredBy = [ "agents-dataset.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    path = [ pkgs.zfs pkgs.coreutils ];
    script = ''
      zpool create -f -O mountpoint=none -O compression=zstd rpool /dev/vdb
      zfs create -o mountpoint=none rpool/safe
      zfs create -o mountpoint=/data rpool/safe/data
      mkdir -p /data/dev/src /data/lab
      chown -R dev:dev /data/dev /data/lab
    '';
  };
}
