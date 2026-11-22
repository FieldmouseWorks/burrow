# Agent sandboxes: a microVM per task, each on its own ZFS clone of the data
# dataset, driven by `agent` (crates/burrow-agent). Opt-in: the project sessions
# and proj jobs keep running on the host unless someone asks for a VM (`agent
# new`, `proj run --vm`). The VM image is agents/guest.nix. With the defaults:
#
#   /agents/ID           rpool/agents/ID: the VM's directory (runner link,
#                        sockets, its writable nix store, metadata)
#   /agents/ID/data      rpool/agents/ID/data: a clone of rpool/safe/data at
#                        the moment the VM was made
#   /agents/ID/home      the VM's home (Claude's sessions and history)
#
# Each engine (burrow.agents.engines, agents/engines.nix) gets an image of its
# own, with its packages alone, and a filtered proxy of its own. Egress goes
# through tinyproxy on 127.0.0.1: an engine's proxyPort allows
# burrow.agents.allowlist, extraAllow and that engine's hosts, 3129 (`agent
# new --open`) allows anything. Nothing else leaves a VM.
#
# An engine's plugin (burrow.agents.engines.NAME.plugin) runs in a VM of the
# same guest made narrower, one per call, with its grants and nothing else
# (agents/plugins.nix).
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.agents;
  zfs = config.boot.zfs.package;
  inherit (config.burrow) user projectsDir;
  account = config.users.users.${user};
  gid = config.users.groups.${account.group}.gid;

  # An engine's guest: the shared one, its packages and modules, nobody
  # else's. `extra` makes it its plugin's (agents/plugin-guest.nix).
  guest = extra: engine: inputs.nixpkgs.lib.nixosSystem {
    modules = [
      (import ./agents/guest.nix inputs)
      {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        time.timeZone = config.time.timeZone;
        burrow.user = user;
        burrow.agentGuest = {
          inherit (account) uid group home;
          inherit gid;
          inherit (engine) packages credentialEnv;
        };
        microvm.mem = cfg.memory;
        microvm.vcpu = cfg.vcpus;
      }
    ] ++ engine.guestModules ++ cfg.guestModules ++ extra;
  };
  runner = engine: (guest [ ] engine).config.microvm.declaredRunner;
  plugins = import ./agents/plugins.nix { inherit inputs config lib pkgs guest; };

  # An engine's filter: the shared list and its own hosts, never another
  # engine's.
  allow = name: engine: pkgs.writeText "agent-allow-${name}.txt"
    (builtins.readFile cfg.allowlist + lib.concatMapStrings (h: "${h}\n") (cfg.extraAllow ++ engine.hosts));

  # QEMU starts one of these per connection a VM opens to its proxy address.
  forward = port: pkgs.writeShellScript "agent-proxy-${toString port}" ''
    exec ${pkgs.socat}/bin/socat - TCP:127.0.0.1:${toString port}
  '';

  # filter: the allowlist file, or null for the open proxy.
  proxy = { port, what, filter }: {
    description = "Egress proxy for agent sandboxes (${what})";
    wantedBy = [ "multi-user.target" ];
    after = [ "network.target" ];
    serviceConfig = {
      ExecStart = "${pkgs.tinyproxy}/bin/tinyproxy -d -c ${pkgs.writeText "agent-proxy-${toString port}.conf" (''
        Port ${toString port}
        Listen 127.0.0.1
        Allow 127.0.0.1
        Timeout 600
        MaxClients 256
        DisableViaHeader Yes
        LogLevel Connect
        ConnectPort 443
      '' + lib.optionalString (filter != null) ''
        Filter "${filter}"
        FilterType fnmatch
        FilterDefaultDeny Yes
      '')}";
      DynamicUser = true;
      Restart = "always";
    };
  };

  # One virtiofsd per share in ./shares (tag, path on the host, path in the VM).
  # Whatever uid the VM writes as, the file is the user's on the host: no root-owned
  # leftovers. virtiofsd exits when QEMU goes away, so each one is respawned
  # for the VM's next boot.
  shares = pkgs.writeShellApplication {
    name = "agent-shares";
    runtimeInputs = [ pkgs.virtiofsd pkgs.coreutils pkgs.systemd ];
    text = ''
      trap 'trap - TERM; kill 0' TERM
      serve() {
        while :; do
          rm -f "$1.sock"
          virtiofsd --socket-path="$1.sock" --socket-group=kvm --shared-dir="$2" \
            --cache=auto --inode-file-handles=prefer \
            --translate-uid squash-guest:0:${toString account.uid}:4294967295 \
            --translate-gid squash-guest:0:${toString gid}:4294967295 || sleep 1
        done
      }
      tags=()
      while IFS=$'\t' read -r tag dir _; do
        serve "$tag" "$dir" &
        tags+=("$tag")
      done <shares
      for tag in "''${tags[@]}"; do
        until [ -S "$tag.sock" ]; do sleep 0.1; done
      done
      systemd-notify --ready
      wait
    '';
  };

  # The binary with its config; it reads every field, and refuses one it
  # doesn't know.
  agentConfig = pkgs.writeText "agent.json" (builtins.toJSON {
    engines = lib.mapAttrs (name: engine: {
      runner = runner engine;
      plugin = plugins.engine name engine;
      forward = forward engine.proxyPort;
      inherit (engine) credentialFile capabilities mcpServers;
    }) cfg.engines;
    # What a plugin could be granted of each server. Never its envVars or
    # host paths, which are a host job's.
    mcpServers = lib.mapAttrs (_: server: {
      inherit (server) command args env actsAsOwner;
    }) config.burrow.mcpServers;
    inherit (cfg) defaultEngine;
    forwardOpen = forward 3129;
    root = cfg.stateDir;
    src = projectsDir;
    data = cfg.dataMountpoint;
    inherit (cfg) dataDataset dataset;
    inherit user;
    inherit (account) uid group home;
    inherit gid;
    plugins = plugins.host;
    sudo = "${config.security.wrapperDir}/sudo";
    path = lib.makeBinPath [
      zfs
      pkgs.coreutils
      pkgs.util-linux
      config.programs.git.package
      pkgs.openssh
      pkgs.socat
      pkgs.systemd
      pkgs.bubblewrap
    ];
  });
  agent = pkgs.runCommand "agent" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "agent";
    passthru.config = agentConfig;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-agent}/bin/burrow-agent \
      $out/bin/agent --add-flags ${agentConfig}
  '';
in
{
  imports = [ (import ./common.nix inputs) inputs.microvm.nixosModules.host ];

  options.burrow.agents = {
    enable = lib.mkEnableOption "agent sandboxes, a microVM per task (`agent`)";

    package = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = agent;
      description = "The configured `agent` wrapper, for modules that stop and start sandboxes.";
    };

    plugins = lib.mkOption {
      type = lib.types.attrsOf lib.types.package;
      readOnly = true;
      default = plugins.executables agent;
      defaultText = lib.literalMD "agent-plugin-NAME for each engine with a plugin";
      description = ''
        By engine, the executable to give a Conary plugin runner: it takes no
        arguments and runs that engine's plugin in the sandbox, with its
        grants (docs/engines.md, "A plugin's sandbox").
      '';
    };

    dataDataset = lib.mkOption {
      type = lib.types.str;
      default = "rpool/safe/data";
      description = "The ZFS dataset each sandbox gets a clone of. burrow.projectsDir must be on it.";
    };
    dataMountpoint = lib.mkOption {
      type = lib.types.str;
      default = "/data";
      description = "Where dataDataset is mounted. Also the only place `agent new --with` takes paths from.";
    };
    dataset = lib.mkOption {
      type = lib.types.str;
      default = "rpool/agents";
      description = "Created on first boot; one child per sandbox.";
    };
    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/agents";
      description = "Where dataset is mounted.";
    };
    allowlist = lib.mkOption {
      type = lib.types.path;
      default = ./agents/allow.txt;
      description = "Hosts every sandbox may reach (tinyproxy fnmatch patterns, one per line); an engine's own are its entry's hosts.";
    };
    extraAllow = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "*.example.com" ];
      description = "More hosts for the allowlist, on top of allowlist.";
    };

    memory = lib.mkOption {
      type = lib.types.int;
      default = 8192;
      description = "MiB of RAM per sandbox (never exactly 2048: QEMU's microvm machine hangs).";
    };
    vcpus = lib.mkOption {
      type = lib.types.int;
      default = 4;
      description = "CPUs per sandbox.";
    };
    guestModules = lib.mkOption {
      type = lib.types.listOf lib.types.deferredModule;
      default = [ ];
      description = "Extra NixOS modules for the sandbox image (agents/guest.nix).";
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
  # Each engine's filtered proxy, agent-proxy-NAME.
  {
    systemd.services = lib.mapAttrs' (name: engine: lib.nameValuePair "agent-proxy-${name}"
      (proxy { port = engine.proxyPort; what = "${name}'s allowlist"; filter = allow name engine; })) cfg.engines;
  }
  # Each engine's plugin: its socket, a service per call, and its executable.
  {
    systemd.sockets = plugins.sockets;
    systemd.services = plugins.services agent;
    environment.systemPackages = lib.attrValues cfg.plugins;
  }
  {
    assertions = [
      {
        assertion = lib.hasPrefix "${cfg.dataMountpoint}/" projectsDir;
        message = "burrow.agents: burrow.projectsDir (${projectsDir}) must be under burrow.agents.dataMountpoint (${cfg.dataMountpoint}), or the sandboxes can't clone it.";
      }
      {
        assertion = account.uid != null && gid != null;
        message = "burrow.agents: give ${user} a fixed uid and its group a fixed gid; the sandboxes use the same ones.";
      }
      {
        assertion = lib.all (n: builtins.match "[a-z0-9][a-z0-9-]*" n != null && n != "open") (lib.attrNames cfg.engines);
        message = "burrow.agents.engines: name each engine in lowercase letters, digits and dashes, and not `open` (its proxy is agent-proxy-NAME).";
      }
      {
        assertion = let ports = lib.mapAttrsToList (_: e: e.proxyPort) cfg.engines;
          in lib.unique ports == ports && !(lib.elem 3129 ports);
        message = "burrow.agents.engines: give each engine a proxyPort of its own, not 3129 (the open proxy's).";
      }
      {
        assertion = cfg.defaultEngine == null || cfg.engines ? ${cfg.defaultEngine};
        message = "burrow.agents.defaultEngine: ${toString cfg.defaultEngine} isn't in burrow.agents.engines.";
      }
    ];

    microvm.stateDir = cfg.stateDir;
    # The sandboxes' directories are microvm:kvm, and the user works in them.
    users.users.${user}.extraGroups = [ "kvm" ];

    environment.systemPackages = [ agent ];
    boot.kernelModules = [ "vhost_vsock" ];

    systemd.services.agents-dataset = {
      description = "Create ${cfg.dataset} for agent sandboxes";
      wantedBy = [ "multi-user.target" ];
      after = [ "zfs-mount.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      path = [ zfs ];
      script = ''
        zfs list -H ${cfg.dataset} >/dev/null 2>&1 ||
          zfs create -o mountpoint=${cfg.stateDir} -o com.sun:auto-snapshot=false ${cfg.dataset}
        chown microvm:kvm ${cfg.stateDir}
        chmod 0775 ${cfg.stateDir}
      '';
    };

    # Every microvm@ here is a sandbox; give it its shares.
    systemd.services."microvm@" = {
      requires = [ "agent-shares@%i.service" ];
      after = [ "agent-shares@%i.service" "agents-dataset.service" ];
    };
    systemd.services."agent-shares@" = {
      description = "Shared folders for agent sandbox %i";
      before = [ "microvm@%i.service" ];
      partOf = [ "microvm@%i.service" ];
      restartIfChanged = false;
      unitConfig.ConditionPathExists = "${cfg.stateDir}/%i/shares";
      serviceConfig = {
        Type = "notify";
        NotifyAccess = "all";
        WorkingDirectory = "${cfg.stateDir}/%i";
        ExecStart = "${shares}/bin/agent-shares";
        LimitNOFILE = 1048576;
        SyslogIdentifier = "agent-shares@%i";
      };
    };

    systemd.services.agent-proxy-open = proxy { port = 3129; what = "open"; filter = null; };

    # Stop sandboxes nobody has used for a day; save and delete the ones past
    # their expiry (crates/burrow-agent).
    systemd.services.agent-gc = {
      description = "Stop idle agent sandboxes, delete expired ones";
      after = [ "agents-dataset.service" ];
      serviceConfig.Type = "oneshot";
      script = "${agent}/bin/agent gc";
    };
    systemd.timers.agent-gc = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = "hourly";
        Persistent = true;
      };
    };

    # Undo inside a sandbox: its clone is snapshotted like everything else, but
    # only kept for a day (the sandbox itself rarely lives longer).
    services.sanoid = {
      enable = lib.mkDefault true;
      templates.agents = {
        frequently = 8;
        hourly = 24;
        daily = 0;
        monthly = 0;
        yearly = 0;
        autosnap = true;
        autoprune = true;
      };
      datasets.${cfg.dataset} = {
        useTemplate = [ "agents" ];
        recursive = true;
      };
    };

    burrow.claude.machineMd = lib.mkOrder 800 ''
      ## Sandboxes, when a task wants one

      Any task can run in its own microVM instead: on a ZFS clone of the project,
      with Claude's permissions bypassed and nothing on the R2 list, because the VM
      is the boundary. Reach for it for risky or sprawling changes, untrusted input
      (someone else's PR, scraped content), or several attempts in parallel. Nothing
      moves into a VM unless someone asks: project sessions and plain proj jobs
      still run on ${config.networking.hostName} itself.

          agent new web                     a fresh sandbox; prints its id (~10s)
          agent claude ID                   Claude inside it, in the project
          agent sh ID [command]             a shell, or one command
          agent fetch ID                    its branches and uncommitted work, into
                                            the real repo under refs/agents/ID/
          agent ls | stop | start | rm | keep
          proj run --vm web "the task"      a proj job inside a fresh sandbox

      A sandbox sees its project (a clone: the real folder never changes), its own
      home, and any `--with ${cfg.dataMountpoint}/...` paths (clones too, e.g. a project's VM
      images; it has KVM). It holds no GitHub token: bring the work back with
      `agent fetch`, then merge and push from ${config.networking.hostName}. Its network goes through
      a proxy that allows the package registries, GitHub and its engine's own hosts
      (burrow.agents.allowlist, and the engine's); `agent new --open` lifts that for one sandbox,
      and adding the host to burrow.agents.extraAllow in the flake is the better
      fix when a project needs a new one. A sandbox unused for a day is stopped;
      after a week its work is fetched and it's deleted. `agent rm` deletes after
      a day, and `agent keep` undoes it.
    '';
  }
  ]);
}
