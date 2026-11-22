# An engine plugin's VM: the shared sandbox's guest (./guest.nix), less
# every way in or out that a plugin's grants don't make. No sshd, no
# per-sandbox mounts, no toolbox, no Nix: crates/burrow-agent attaches what
# one run is granted (src/plugin/vm.rs), and its /nix/store is this image's
# closure with the granted tools', never the host's store.
#
# The user still has root in here, as in every sandbox: nothing in this file
# is a guard.
{ package, program }:
{ config, lib, pkgs, ... }:
let
  inherit (config.burrow) user;
  guest = config.burrow.agentGuest;
  bin = "${package}/bin/burrow-agent-guest";
in
{
  burrow.toolbox.enable = lib.mkForce false;
  nix.enable = lib.mkForce false;
  system.disableInstallerTools = true;
  environment.defaultPackages = lib.mkForce [ ];
  environment.systemPackages = [
    pkgs.socat
    (pkgs.writeShellScriptBin "heavy" ''exec ${bin} heavy "$@"'')
  ];

  microvm = {
    # No QMP socket, writable store or disk: a run leaves nothing behind.
    socket = lib.mkForce null;
    writableStoreOverlay = lib.mkForce null;
    volumes = lib.mkForce [ ];
  };
  services.openssh.enable = lib.mkForce false;
  systemd.services.agent-mounts.enable = lib.mkForce false;
  systemd.services.agent-init.enable = lib.mkForce false;

  # Its NIC is there only with a forward to reach, so nothing waits for it.
  systemd.network.wait-online.enable = false;
  systemd.network.networks."10-slirp" = {
    networkConfig = {
      DHCP = lib.mkForce "no";
      IPv6AcceptRA = false;
    };
    address = [ "10.0.2.15/24" ];
    gateway = [ "10.0.2.2" ];
  };

  systemd.services.agent-plugin = {
    description = "The engine's plugin, for the host's one request";
    wantedBy = [ "multi-user.target" ];
    after = [ "local-fs.target" "systemd-udevd.service" ];
    path = [ pkgs.util-linux pkgs.socat ];
    serviceConfig = {
      ExecStart = lib.escapeShellArgs ([ bin "plugin" program (toString guest.uid) (toString guest.gid) ]
        ++ lib.optional (guest.credentialEnv != null) guest.credentialEnv);
      ImportCredential = "agent.credential";
      WorkingDirectory = "/";
      SyslogIdentifier = "agent-plugin-${user}";
    };
  };
}
