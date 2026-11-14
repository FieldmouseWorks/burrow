# builds: the machine's build store (docs/gate-design.md, "One build store,
# written by whoever may sign"), a Nix binary cache nginx serves as files,
# at https://HOST/PATH, signed with the machine's own key.
#
# Reading is public and static. Writing goes through the door
# (crates/burrow-builds): a run POSTs its manifest, NARs and its job's own
# GitHub OIDC token to PATH/upload, nginx hands the request to one
# socket-activated door, and the door asks Conary (`cn attest job`, as the
# ledger's writers run cn) whether the job may sign, then takes only a public repo
# this machine lists, and writes each NAR only once its hash is the
# manifest's. Past `maxSize` it drops what nginx served longest ago: nginx
# appends a line to fetched/NAR each time it serves that NAR.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.builds;
  inherit (lib) mkOption types;
  account = "burrow-builds";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-builds;
  ledger = config.burrow.ledger;
  app = config.burrow.github.machineApp;
  keyDir = "${config.burrow.stateDir}/builds-key";
  socket = "/run/burrow-builds/door.sock";
  path = lib.removeSuffix "/" cfg.path;

  # What crates/burrow-builds reads (its door::Config).
  configFile = pkgs.writeText "burrow-builds.json" (builtins.toJSON {
    inherit (cfg) dir repos;
    key = "${keyDir}/secret-key";
    key_name = cfg.keyName;
    max_size = cfg.maxSize;
    # cn as the ledger's writers run it: as the machine's App, under the
    # machine's signer workflow and fresh runners, as the train's.
    cn = [ (lib.getExe ledger.writer) ];
  });

  hardening = {
    User = account;
    Group = account;
    UMask = "0022";
    ProtectSystem = "strict";
    ProtectHome = true;
    PrivateTmp = true;
    PrivateDevices = true;
    ProtectKernelTunables = true;
    ProtectKernelModules = true;
    ProtectKernelLogs = true;
    ProtectControlGroups = true;
    ProtectClock = true;
    ProtectHostname = true;
    RestrictNamespaces = true;
    RestrictRealtime = true;
    RestrictSUIDSGID = true;
    LockPersonality = true;
    NoNewPrivileges = true;
    CapabilityBoundingSet = "";
    SystemCallArchitectures = "native";
  };
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; }
    { key = "burrow/ledger"; imports = [ (import ./ledger.nix inputs) ]; }
  ];

  options.burrow.builds = {
    enable = lib.mkEnableOption "the machine's build store, which runs that may sign write";

    host = mkOption {
      type = types.str;
      example = "example.org";
      description = "The nginx virtual host the store is served at, and its door.";
    };

    path = mkOption {
      type = types.strMatching "/[^ ]*";
      default = "/builds";
      description = "Where on `host` the store is: a substituter's URL is https://HOST/PATH, its door PATH/upload.";
    };

    dir = mkOption {
      type = types.str;
      default = "/var/lib/burrow-builds";
      description = ''
        The cache: its narinfos, NARs and public key, which nginx serves as
        they are. Persist it if / is ephemeral, or every run after a boot
        builds cold.
      '';
    };

    keyName = mkOption {
      type = types.strMatching "[^: ]+";
      default = "${config.networking.hostName}-builds-1";
      defaultText = lib.literalExpression ''"''${config.networking.hostName}-builds-1"'';
      description = ''
        The signing key's name, as a store that trusts it names it. The key is
        made on first start under burrow.stateDir (persist that), never handed
        to a run; its public half is served at PATH/public-key.
      '';
    };

    maxSize = mkOption {
      type = types.ints.positive;
      default = 100 * 1024 * 1024 * 1024;
      defaultText = lib.literalExpression "100 GiB";
      description = "The most bytes its NARs and narinfos take; past it, what was fetched longest ago goes.";
    };

    maxUpload = mkOption {
      type = types.str;
      default = "20g";
      description = "The largest upload nginx takes (its client_max_body_size).";
    };

    repos = mkOption {
      type = types.listOf (types.strMatching "[^/]+/[^/]+");
      default = [ ];
      example = [ "sam/homelab" ];
      description = ''
        The repos (OWNER/NAME) whose runs may write it. A private repo's are
        refused whatever this says: its runs are on the machine, whose store
        they share already, and what the store holds is public.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = ledger.enable;
        message = "burrow.builds asks Conary's cn (burrow.ledger.package) whether a job may write: it needs burrow.ledger.";
      }
      {
        assertion = cfg.repos != [ ];
        message = "burrow.builds.repos: name the repos whose runs may write the store.";
      }
      {
        assertion = app != null && builtins.elem account app.users;
        message = "burrow.builds reads GitHub as the machine's App: it needs burrow.github.machinery, with ${account} among its users.";
      }
    ];

    users.users.${account} = {
      isSystemUser = true;
      group = account;
    };
    users.groups.${account} = { };

    # Beside the App's default askers, not instead of them.
    burrow.github.apps = let machinery = config.burrow.github.machinery; in
      lib.mkIf (machinery != null) {
        ${machinery}.users = lib.mkOptionDefault [ account ];
      };

    # The cache for nginx to read; fetched/ for it to write each fetch to;
    # the key for the door alone.
    systemd.tmpfiles.rules = [
      "d ${cfg.dir} 0755 ${account} ${account} -"
      "d ${cfg.dir}/fetched 2770 ${account} ${config.services.nginx.group} -"
      "d ${keyDir} 0700 ${account} ${account} -"
    ];

    systemd.services."${account}-init" = {
      description = "The build store's directories, and its key on first start";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-tmpfiles-setup.service" ];
      serviceConfig = hardening // {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe package} init ${configFile}";
        ReadWritePaths = [ cfg.dir keyDir ];
        PrivateNetwork = true;
      };
    };

    # One door a connection, from nginx alone.
    systemd.sockets.${account} = {
      description = "The build store's door";
      wantedBy = [ "sockets.target" ];
      requires = [ "${account}-init.service" ];
      after = [ "${account}-init.service" ];
      listenStreams = [ socket ];
      socketConfig = {
        Accept = true;
        SocketUser = account;
        SocketGroup = config.services.nginx.group;
        SocketMode = "0660";
        MaxConnections = 8;
      };
    };

    systemd.services."${account}@" = {
      description = "An upload to the build store";
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe package} door ${configFile}";
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        ReadWritePaths = [ cfg.dir ];
        # cn asks GitHub, and the App's token socket.
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RuntimeMaxSec = "2h";
      };
    };

    services.nginx = {
      enable = true;
      # A line a fetch, which is all the door reads of it (its time).
      commonHttpConfig = ''
        log_format burrow_builds_fetched '-';
      '';
      virtualHosts.${cfg.host}.locations = {
        "${path}/" = {
          alias = "${cfg.dir}/";
          extraConfig = ''
            location ~ ^${lib.escapeRegex path}/(fetched|tmp)/ { return 404; }
            location ~ ^${lib.escapeRegex path}/nar/(?<burrow_builds_nar>[0-9a-z]{32}\.nar)$ {
              alias ${cfg.dir}/nar/$burrow_builds_nar;
              access_log ${cfg.dir}/fetched/$burrow_builds_nar burrow_builds_fetched;
            }
          '';
        };
        "= ${path}/upload" = {
          proxyPass = "http://unix:${socket}:";
          extraConfig = ''
            client_max_body_size ${cfg.maxUpload};
            proxy_request_buffering on;
            proxy_http_version 1.0;
            # A door waits for another upload's lock, or for cn, without
            # reading: nginx holds on as long either way.
            proxy_send_timeout 2h;
            proxy_read_timeout 2h;
          '';
        };
      };
    };
    # nginx's own unit writes only where it's let.
    systemd.services.nginx.serviceConfig.ReadWritePaths = [ "${cfg.dir}/fetched" ];
  };
}
