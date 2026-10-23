# Agent sandboxes: a microVM per task, each on its own ZFS clone of the data
# dataset, driven by `agent` (agents/agent.sh). Opt-in: the project sessions
# and proj jobs keep running on the host unless someone asks for a VM (`agent
# new`, `proj run --vm`). The VM image is agents/guest.nix. With the defaults:
#
#   /agents/ID           rpool/agents/ID: the VM's directory (runner link,
#                        sockets, its writable nix store, metadata)
#   /agents/ID/data      rpool/agents/ID/data: a clone of rpool/safe/data at
#                        the moment the VM was made
#   /agents/ID/home      the VM's home (Claude's sessions and history)
#
# Egress goes through tinyproxy on 127.0.0.1: 3128 allows burrow.agents.allowlist,
# 3129 (`agent new --open`) allows anything. Nothing else leaves a VM.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.agents;
  zfs = config.boot.zfs.package;
  inherit (config.burrow) user projectsDir;
  account = config.users.users.${user};
  gid = config.users.groups.${account.group}.gid;

  guest = inputs.nixpkgs.lib.nixosSystem {
    modules = [
      (import ./agents/guest.nix inputs)
      {
        nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
        time.timeZone = config.time.timeZone;
        burrow.user = user;
        burrow.agentGuest = {
          inherit (account) uid group home;
          inherit gid;
          claudePackage = config.burrow.claude.package;
        };
        microvm.mem = cfg.memory;
        microvm.vcpu = cfg.vcpus;
      }
    ] ++ cfg.guestModules;
  };
  runner = guest.config.microvm.declaredRunner;

  allow = pkgs.writeText "agent-allow.txt"
    (builtins.readFile cfg.allowlist + lib.concatMapStrings (h: "${h}\n") cfg.extraAllow);

  # QEMU starts one of these per connection a VM opens to its proxy address.
  forward = port: pkgs.writeShellScript "agent-proxy-${toString port}" ''
    exec ${pkgs.socat}/bin/socat - TCP:127.0.0.1:${toString port}
  '';

  proxy = { port, filtered }: {
    description = "Egress proxy for agent sandboxes (${if filtered then "allowlist" else "open"})";
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
      '' + lib.optionalString filtered ''
        Filter "${allow}"
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

  agent = pkgs.writeShellApplication {
    name = "agent";
    runtimeInputs = [
      zfs
      pkgs.coreutils
      pkgs.util-linux
      pkgs.procps
      pkgs.gnused
      pkgs.jq
      pkgs.git
      pkgs.openssh
      pkgs.socat
      pkgs.systemd
    ];
    text = ''
      AGENT_RUNNER=${runner}
      AGENT_FWD_FILTERED=${forward 3128}
      AGENT_FWD_OPEN=${forward 3129}
      AGENT_ROOT=${lib.escapeShellArg cfg.stateDir}
      AGENT_SRC=${lib.escapeShellArg projectsDir}
      AGENT_DATA=${lib.escapeShellArg cfg.dataMountpoint}
      AGENT_DATA_DATASET=${lib.escapeShellArg cfg.dataDataset}
      AGENT_DATASET=${lib.escapeShellArg cfg.dataset}
      AGENT_USER=${lib.escapeShellArg user}
      AGENT_GROUP=${lib.escapeShellArg account.group}
      AGENT_HOME=${lib.escapeShellArg account.home}
      AGENT_TOKEN_FILE=${lib.escapeShellArg (if cfg.claudeTokenFile == null then "" else cfg.claudeTokenFile)}
    '' + builtins.readFile ./agents/agent.sh;
  };
in
{
  imports = [ (import ./common.nix inputs) inputs.microvm.nixosModules.host ];

  options.burrow.agents = {
    enable = lib.mkEnableOption "agent sandboxes, a microVM per task (`agent`)";

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
      description = "Hosts a sandbox may reach (tinyproxy fnmatch patterns, one per line).";
    };
    extraAllow = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "*.example.com" ];
      description = "More hosts for the allowlist, on top of allowlist.";
    };
    claudeTokenFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/claude-agent-token";
      description = ''
        Claude's login for the sandboxes: a token from `claude setup-token`.
        Never hand VMs the host's own ~/.claude/.credentials.json: its refresh
        token rotates, and a VM refreshing it can log out the host's sessions.
      '';
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

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.hasPrefix "${cfg.dataMountpoint}/" projectsDir;
        message = "burrow.agents: burrow.projectsDir (${projectsDir}) must be under burrow.agents.dataMountpoint (${cfg.dataMountpoint}), or the sandboxes can't clone it.";
      }
      {
        assertion = account.uid != null && gid != null;
        message = "burrow.agents: give ${user} a fixed uid and its group a fixed gid; the sandboxes use the same ones.";
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

    systemd.services.agent-proxy = proxy { port = 3128; filtered = true; };
    systemd.services.agent-proxy-open = proxy { port = 3129; filtered = false; };

    # Stop sandboxes nobody has used for a day; save and delete the ones past
    # their expiry (agent.sh).
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
      a proxy that allows the package registries, GitHub and Anthropic
      (burrow.agents.allowlist); `agent new --open` lifts that for one sandbox,
      and adding the host to burrow.agents.extraAllow in the flake is the better
      fix when a project needs a new one. A sandbox unused for a day is stopped;
      after a week its work is fetched and it's deleted. `agent rm` deletes after
      a day, and `agent keep` undoes it.
    '';
  };
}
