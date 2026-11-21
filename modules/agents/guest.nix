# The agent sandbox: a microVM that `agent new` (../agents.nix) boots for one
# task, on its own ZFS clone of the data dataset. ../agents.nix builds one
# image of it per engine (burrow.agents.engines), with the host's user and
# time zone, that engine's packages and its guest modules, and no other
# engine's. What differs per VM (which project it sees, its vsock address,
# its network policy, its engine's login) comes in at boot as systemd
# credentials and QEMU arguments that `agent` writes next to the VM, so
# starting one builds nothing.
#
# Inside, the engine runs with nothing to ask and the user has root: the VM
# is the boundary. What it can touch outside is the project folder (a clone),
# its own home, and the egress proxy. Files it writes land on the host as the
# user's, whichever user wrote them.
inputs:
{ lib, pkgs, config, ... }:
let
  cfg = config.burrow.agentGuest;
  inherit (config.burrow) user;

  # Through the proxy only (../agents.nix): QEMU forwards this one address to
  # it, and nothing else leaves the VM.
  proxy = "http://10.0.2.100:3128";
in
{
  imports = [
    inputs.microvm.nixosModules.microvm
    (import ../toolbox.nix inputs)
  ];

  options.burrow.agentGuest = {
    consoleLevel = lib.mkOption {
      type = lib.types.str;
      default = "warning";
      description = "What the guest's journal copies to its console (the host's journal).";
    };
    # The rest mirror the host (../agents.nix sets them).
    uid = lib.mkOption { type = lib.types.int; };
    group = lib.mkOption { type = lib.types.str; };
    gid = lib.mkOption { type = lib.types.int; };
    home = lib.mkOption { type = lib.types.str; };
    # The engine's (burrow.agents.engines.NAME): what it installs, and the
    # variable its credential becomes.
    packages = lib.mkOption { type = lib.types.listOf lib.types.package; };
    credentialEnv = lib.mkOption { type = lib.types.nullOr lib.types.str; };
  };

  config = {
    burrow.toolbox.enable = true;

    networking.hostName = "agent";
    system.stateVersion = "26.05";
    nixpkgs.config.allowUnfree = true;

    microvm = {
      hypervisor = "qemu";
      # The size of one sandbox (burrow.agents.memory and vcpus); the host's
      # sessions, the ZFS ARC and these all share its RAM, and the balloon hands
      # back whatever a VM isn't using.
      vcpu = lib.mkDefault 4;
      mem = lib.mkDefault 8192;
      balloon = true;
      deflateOnOOM = true;

      # Relative paths resolve in the VM's own directory, STATEDIR/<id>.
      socket = "control.sock";
      shares = [
        {
          tag = "ro-store";
          source = "/nix/store";
          mountPoint = "/nix/.ro-store";
          proto = "virtiofs";
          socket = "ro-store.sock";
          readOnly = true;
        }
      ];
      # `nix shell nixpkgs#foo` works inside: new paths go to this overlay, which
      # lives and dies with the VM's clone.
      writableStoreOverlay = "/nix/.rw-store";
      volumes = [
        {
          image = "nix-rw.img";
          label = "nix-rw";
          mountPoint = "/nix/.rw-store";
          size = 40960;
        }
      ];

      # The per-VM part: the network, vsock, the project and home shares and the
      # credentials. `agent new` writes it; see crates/burrow-agent.
      extraArgsScript = "${pkgs.coreutils}/bin/cat qemu-args";
    };

    # Credentials arrive over fw_cfg, which PID 1 reads at start.
    boot.initrd.kernelModules = [ "qemu_fw_cfg" ];
    # Udev loads the CPU's KVM module for nested VMs on either CPU vendor.
    boot.kernelModules = [ "vmw_vsock_virtio_transport" ];

    # Mount what `agent new` shared: the project's clone at the same path it has
    # on the host (absolute paths in node_modules, .venv and scripts keep working),
    # the VM's own home, and any --with paths. Lines are "tag<TAB>mountpoint".
    systemd.services.agent-mounts = {
      description = "Mount this agent's shares";
      wantedBy = [ "local-fs.target" ];
      before = [ "local-fs.target" ];
      after = [ "systemd-modules-load.service" ];
      unitConfig.DefaultDependencies = false;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ImportCredential = "agent.mounts";
      };
      path = [ pkgs.util-linux pkgs.coreutils ];
      script = ''
        while IFS=$'\t' read -r tag dir; do
          [ -n "$tag" ] || continue
          mkdir -p "$dir"
          mount -t virtiofs "$tag" "$dir"
        done <"$CREDENTIALS_DIRECTORY/agent.mounts"
      '';
    };

    # The VM's name, and its engine's login if the host has one for agents.
    systemd.services.agent-init = {
      description = "Name this agent and hand ${user} its engine's login";
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ImportCredential = [ "agent.id" "agent.credential" ];
      };
      path = [ pkgs.coreutils pkgs.hostname ];
      script = ''
        if [ -s "$CREDENTIALS_DIRECTORY/agent.id" ]; then
          hostname "$(cat "$CREDENTIALS_DIRECTORY/agent.id")"
        fi
        mkdir -p /run/agent
        if [ -s "$CREDENTIALS_DIRECTORY/agent.credential" ]; then
          install -m 0400 -o ${user} -g ${cfg.group} "$CREDENTIALS_DIRECTORY/agent.credential" /run/agent/credential
        fi
      '';
    };
    environment.extraInit = lib.optionalString (cfg.credentialEnv != null) ''
      if [ -r /run/agent/credential ]; then
        ${cfg.credentialEnv}=$(cat /run/agent/credential)
        export ${cfg.credentialEnv}
      fi
    '';

    users.mutableUsers = false;
    # The user's key arrives in its home at boot, which the build can't see.
    users.allowNoPasswordLogin = true;
    users.users.${user} = {
      isNormalUser = true;
      inherit (cfg) uid group home;
      extraGroups = [ "wheel" "kvm" ];
      shell = pkgs.bash;
      hashedPassword = "!";
    };
    users.groups.${cfg.group}.gid = cfg.gid;
    users.users.root.hashedPassword = "!";
    security.sudo.wheelNeedsPassword = false;
    security.sudo.extraConfig = ''
      Defaults lecture = never
    '';

    # The way in: sshd on vsock port 22, keys from ~/.ssh/authorized_keys (which
    # `agent new` puts in the VM's home). systemd's ssh generator opens the vsock
    # listener by itself; the TCP one is unreachable (nothing can get in).
    services.openssh = {
      enable = true;
      openFirewall = false;
      settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
        PermitRootLogin = "no";
      };
    };

    # Warnings and auth failures go to the console, which is the host's
    # `journalctl -u microvm@ID`.
    services.journald.extraConfig = ''
      ForwardToConsole=yes
      MaxLevelConsole=${cfg.consoleLevel}
    '';

    # Slirp's DHCP gives 10.0.2.15; there's no DNS to ask (the proxy resolves),
    # and nothing can reach in.
    networking.useDHCP = false;
    networking.useNetworkd = true;
    systemd.network.networks."10-slirp" = {
      matchConfig.Type = "ether";
      networkConfig.DHCP = "ipv4";
      dhcpV4Config.UseDNS = false;
    };
    networking.nameservers = lib.mkForce [ ];
    services.resolved.enable = false;
    networking.firewall.enable = false;
    networking.proxy = {
      default = proxy;
      noProxy = "127.0.0.1,localhost";
    };
    environment.variables = {
      HTTP_PROXY = proxy;
      HTTPS_PROXY = proxy;
      NO_PROXY = "127.0.0.1,localhost";
    };

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      trusted-users = [ "root" "@wheel" ];
    };
    nix.channel.enable = false;

    # proj's guest side, the engine's packages and a few tools.
    environment.systemPackages = [
      (pkgs.runCommand "proj-guest" {} ''
        mkdir -p $out/bin
        ln -s ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj-guest $out/bin/proj
      '')
      pkgs.curl
      pkgs.jq
      pkgs.htop
      pkgs.vim
    ] ++ cfg.packages;

    documentation.enable = false;
    i18n.defaultLocale = "en_US.UTF-8";
  };
}
