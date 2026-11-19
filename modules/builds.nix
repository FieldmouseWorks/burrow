# builds: the machine's build store (docs/gate-design.md, "One build store,
# written by whoever may sign"), a Nix binary cache nginx serves as files,
# at https://HOST/PATH, signed with the machine's own key.
#
# Reading is public and static. Writing goes through the door
# (crates/burrow-builds): a run POSTs its manifest, NARs and its job's own
# GitHub OIDC token to PATH/upload. Before nginx takes the body it asks a
# door of its own (`auth_request`, from the head alone) whether the token
# is a listed public repo's job's, so a client without one is refused
# before nginx buffers a byte. Then nginx hands the request to one
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
  # Its own, so uploads parked on the store's lock never hold every check
  # off: each upload takes one of each in turn, never both at once.
  admitSocket = "/run/burrow-builds/admit.sock";
  path = lib.removeSuffix "/" cfg.path;

  # The directories above `dir` this config declares owned by a user other
  # than root or the account: tmpfiles refuses the path through them. Only
  # what it declares; a rule it can't read plainly is left out, never flagged.
  owners = [ "-" "root" "0" account ];
  above = p: let d = lib.removeSuffix "/" p; in d != "" && lib.hasPrefix "${d}/" cfg.dir;
  ruleParents = lib.concatMap (r:
    let f = lib.filter (x: x != "") (lib.splitString " " (lib.replaceStrings [ "\t" ] [ " " ] r));
    in lib.optional (lib.length f >= 4 && !(lib.hasPrefix "#" (lib.head f))) { path = lib.elemAt f 1; user = lib.elemAt f 3; }
  ) config.systemd.tmpfiles.rules;
  settingParents = lib.concatMap (paths:
    lib.concatLists (lib.mapAttrsToList (p: entries: map (e: { path = p; user = e.user or "-"; }) (lib.attrValues entries)) paths)
  ) (lib.attrValues config.systemd.tmpfiles.settings);
  homeParents = map (u: { path = u.home; user = u.name; })
    (lib.filter (u: u.createHome) (lib.attrValues config.users.users));
  unsafeParents = lib.filter (e: above e.path && !(lib.elem e.user owners)) (ruleParents ++ settingParents ++ homeParents);

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

        Every directory above it must be owned by root (or by burrow-builds):
        systemd-tmpfiles won't make it, or its fetched/, through another
        user's directory (an "unsafe path transition"), and without fetched/
        the store keeps no record of what it served. Evaluation fails when
        the machine's own config declares such a parent (a tmpfiles entry,
        or a user's home it makes); one owned so on disk alone it can't see.
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
      description = ''
        The largest upload nginx takes (its client_max_body_size) from a
        job whose token the door has already checked from the request's
        head: a client without one is refused before nginx reads its body.
      '';
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
      {
        assertion = unsafeParents == [ ];
        message = "burrow.builds.dir is ${cfg.dir}, under ${lib.concatMapStringsSep ", " (e: "${e.path} (declared owned by ${e.user})") unsafeParents}: systemd-tmpfiles won't make it through another user's directory. Put it where every parent is root's (the option's description says why).";
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

    # One door a connection, from nginx alone. The socket listens from
    # sockets.target, before the init service can run (it comes after
    # basic.target): each door waits for it instead.
    systemd.sockets.${account} = {
      description = "The build store's door";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ socket ];
      socketConfig = {
        Accept = true;
        SocketUser = account;
        SocketGroup = config.services.nginx.group;
        SocketMode = "0660";
        MaxConnections = 8;
      };
    };

    # nginx's check of an upload's head, before it takes the body: quick,
    # and with nothing of the store's to write.
    systemd.sockets."${account}-admit" = {
      description = "The build store's check of an upload's token";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ admitSocket ];
      socketConfig = {
        Accept = true;
        SocketUser = account;
        SocketGroup = config.services.nginx.group;
        SocketMode = "0660";
        MaxConnections = 8;
      };
    };

    systemd.services."${account}-admit@" = {
      description = "A check of an upload's token";
      serviceConfig = hardening // {
        ExecStart = "${lib.getExe package} admit ${configFile}";
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        # cn asks GitHub, and the App's token socket.
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RuntimeMaxSec = "2min";
      };
    };

    systemd.services."${account}@" = {
      description = "An upload to the build store";
      requires = [ "${account}-init.service" ];
      after = [ "${account}-init.service" ];
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
            # Quoted: nginx ends an unquoted regex at its `{`.
            location ~ "^${lib.escapeRegex path}/nar/(?<burrow_builds_nar>[0-9a-z]{32}\.nar)$" {
              alias ${cfg.dir}/nar/$burrow_builds_nar;
              access_log ${cfg.dir}/fetched/$burrow_builds_nar burrow_builds_fetched;
            }
          '';
        };
        "= ${path}/upload" = {
          proxyPass = "http://unix:${socket}:";
          extraConfig = ''
            # The token, from the head, before nginx reads the body; the door
            # holds it to the manifest once it has that.
            auth_request ${path}/upload-admit;
            auth_request_set $burrow_builds_admit $upstream_status;
            auth_request_set $burrow_builds_refused $upstream_http_burrow_refused;
            error_page 401 403 500 = @burrow_builds_refused;
            client_max_body_size ${cfg.maxUpload};
            proxy_request_buffering on;
            proxy_http_version 1.0;
            # A door waits for another upload's lock, or for cn, without
            # reading: nginx holds on as long either way.
            proxy_send_timeout 2h;
            proxy_read_timeout 2h;
          '';
        };
        "= ${path}/upload-admit" = {
          proxyPass = "http://unix:${admitSocket}:";
          extraConfig = ''
            internal;
            # It sees the upload's Content-Length but never its body.
            client_max_body_size 0;
            proxy_pass_request_body off;
            proxy_set_header Content-Length "";
            proxy_http_version 1.0;
            proxy_send_timeout 2m;
            proxy_read_timeout 2m;
          '';
        };
        # auth_request passes on only a 401 or 403, with no body, and makes
        # anything else a 500: this gives the client the door's own answer.
        # A door that couldn't be asked is one to try again, as cn's GitHub
        # away is; a 500 the door didn't give is nginx's own.
        "@burrow_builds_refused".extraConfig = ''
          default_type application/json;
          if ($burrow_builds_admit = 401) { return 401 "$burrow_builds_refused\n"; }
          if ($burrow_builds_admit = 403) { return 403 "$burrow_builds_refused\n"; }
          if ($burrow_builds_admit = 500) { return 500 "$burrow_builds_refused\n"; }
          if ($burrow_builds_admit = 503) { return 503 "$burrow_builds_refused\n"; }
          if ($burrow_builds_admit ~ "^50[24]") {
            return 503 "{\"refused\":\"the door that checks a token is busy or down; try again\"}\n";
          }
          return 500 "{\"refused\":\"nginx failed the upload: its log says why\"}\n";
        '';
      };
    };
    # nginx's own unit writes only where it's let. Optional (`-`): with
    # fetched/ missing, systemd would refuse to start nginx and every site
    # on it would go down; this way only the store's fetch log is lost.
    systemd.services.nginx.serviceConfig.ReadWritePaths = [ "-${cfg.dir}/fetched" ];
  };
}
