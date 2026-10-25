# events: what happens on GitHub, or on this machine, starts the work it
# calls for within seconds (events/events.py). Nothing here runs on a timer
# but the hourly catch-up, for what no event reports.
#
# GitHub posts each repo's events to https://HOST/PATH (the steward keeps
# every repo's hook, autoland/repos.py); nginx passes them to burrow-events,
# which checks each one's signature and starts the pass of every handler that
# takes that event, for that repo. Things on this machine kick a handler on
# the local port (`curl -d '' http://127.0.0.1:KICKPORT/kick/HANDLER/OWNER/REPO`),
# and a pass books its own next one for when a wait it saw ends.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.events;
  inherit (lib) mkOption types;

  configFile = pkgs.writeText "burrow-events.json" (builtins.toJSON {
    inherit (cfg) port kickPort ownStatuses;
    handlers = lib.mapAttrs (_: h: { inherit (h) events unit wake; }) cfg.handlers;
  });
  src = pkgs.runCommand "burrow-events" { nativeBuildInputs = [ pkgs.python3.pkgs.flake8 ]; } ''
    mkdir $out
    cp ${./events/events.py} $out/events.py
    flake8 --ignore E501,W503 $out
  '';
  units = lib.mapAttrsToList (_: h: h.unit) cfg.handlers;
  kickAll = lib.concatMapStrings (name: ''
    curl -fsS -m 10 -d "" ${cfg.kick}/${name} >/dev/null
  '') (lib.attrNames cfg.handlers);
in
{
  options.burrow.events = {
    enable = lib.mkEnableOption ''
      burrow-events: GitHub's webhook deliveries, local kicks and the passes'
      own wake-ups start each handler's pass. nginx on this machine has to
      serve `host` over HTTPS'';

    host = mkOption {
      type = types.str;
      example = "example.org";
      description = "The nginx virtual host GitHub reaches this machine at: the hooks' URL is https://HOST/PATH.";
    };

    path = mkOption {
      type = types.str;
      default = "/burrow/hook";
      description = "Where on `host` the deliveries go; nginx passes them to burrow-events.";
    };

    port = mkOption {
      type = types.port;
      default = 8787;
      description = "The local port burrow-events takes deliveries on, from nginx (127.0.0.1 only).";
    };

    kickPort = mkOption {
      type = types.port;
      default = 8788;
      description = "The local port it takes kicks on (127.0.0.1 only; nginx never passes anything to it).";
    };

    secretFile = mkOption {
      type = types.str;
      example = "/run/secrets/burrow-webhook";
      description = ''
        A root-only file holding the secret GitHub signs each delivery with: a
        delivery signed with anything else is turned away. The steward gives
        it to every repo's hook, so changing it re-sets them.
      '';
    };

    handlers = mkOption {
      default = { };
      description = ''
        What burrow-events starts, by name (the NAME a kick gives). UNIT.service
        is the handler's full pass and UNIT@.service, a template, its pass over
        the one repo its instance names (`%I` is OWNER/REPO).
      '';
      type = types.attrsOf (types.submodule {
        options = {
          events = mkOption {
            type = types.listOf types.str;
            description = "The GitHub events (X-GitHub-Event) that start the repo's pass.";
          };
          unit = mkOption {
            type = types.str;
            description = "The units' name, without @ or .service.";
          };
          wake = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = ''
              A JSON file, {"OWNER/REPO": epoch}, the handler's passes write:
              when each repo next needs a pass. Read after each pass, and when
              burrow-events starts.
            '';
          };
        };
      });
    };

    ownStatuses = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = ''
        The commit status contexts this machine posts itself (the steward's
        `review-settled`): a `status` delivery for one says nothing new, so it
        starts no pass.
      '';
    };

    url = mkOption {
      type = types.str;
      readOnly = true;
      default = "https://${cfg.host}${cfg.path}";
      defaultText = lib.literalExpression ''"https://''${host}''${path}"'';
      description = "The hooks' URL.";
    };

    kick = mkOption {
      type = types.str;
      readOnly = true;
      default = "http://127.0.0.1:${toString cfg.kickPort}/kick";
      defaultText = lib.literalExpression ''"http://127.0.0.1:''${toString kickPort}/kick"'';
      description = "Where to kick a handler: POST to KICK/HANDLER or KICK/HANDLER/OWNER/REPO.";
    };

    hookEvents = mkOption {
      type = types.listOf types.str;
      readOnly = true;
      default = lib.unique (lib.sort lib.lessThan (lib.concatMap (h: h.events) (lib.attrValues cfg.handlers)));
      description = "Every event a handler takes: what each repo's hook sends.";
    };
  };

  config = lib.mkIf cfg.enable {
    users.users.burrow-events = {
      isSystemUser = true;
      group = "burrow-events";
    };
    users.groups.burrow-events = { };

    systemd.services.burrow-events = {
      description = "burrow-events: a pass on each GitHub event, kick or wake-up";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];
      path = [ config.systemd.package ];
      environment.EVENTS_CONFIG = "${configFile}";
      serviceConfig = {
        User = "burrow-events";
        ExecStart = "${pkgs.python3}/bin/python3 -E -s ${src}/events.py";
        Restart = "always";
        RestartSec = 5;
        LoadCredential = "webhook:${cfg.secretFile}";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
      };
    };

    # A new secret (secretFile written, or replaced as sops does) restarts the
    # service, which reads it only when it starts. Its catch-up full passes
    # then give every repo's hook the new one.
    systemd.paths.burrow-events-secret = {
      wantedBy = [ "multi-user.target" ];
      pathConfig.PathChanged = cfg.secretFile;
    };
    systemd.services.burrow-events-secret = {
      description = "burrow-events: restart for a new webhook secret";
      path = [ config.systemd.package ];
      script = "systemctl try-restart burrow-events.service";
      serviceConfig.Type = "oneshot";
    };

    # The only timer: a full pass of every handler each hour, for what no
    # event reports. A delivery GitHub couldn't make (this machine down, or
    # nginx) is never sent again, and a repo made on a user account sends
    # nothing before its first pass gives it a hook. A pass that has nothing
    # to do asks GitHub for little: one listing, a few points.
    systemd.services.burrow-events-catch-up = {
      description = "burrow-events: a full pass of every handler, for what no event reported";
      path = [ pkgs.curl ];
      script = kickAll;
      serviceConfig = {
        Type = "oneshot";
        User = "burrow-events";
      };
    };
    systemd.timers.burrow-events-catch-up = {
      wantedBy = [ "timers.target" ];
      timerConfig.OnCalendar = "hourly";
    };

    # burrow-events starts the handlers' passes, and nothing else.
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id != "org.freedesktop.systemd1.manage-units" || subject.user != "burrow-events"
            || action.lookup("verb") != "start") return polkit.Result.NOT_HANDLED;
        var unit = action.lookup("unit"), units = ${builtins.toJSON units};
        for (var i = 0; i < units.length; i++) {
          if (unit == units[i] + ".service") return polkit.Result.YES;
          if (unit.indexOf(units[i] + "@") == 0 && unit.slice(-8) == ".service") return polkit.Result.YES;
        }
        return polkit.Result.NOT_HANDLED;
      });
    '';

    services.nginx.virtualHosts.${cfg.host}.locations.${cfg.path} = {
      proxyPass = "http://127.0.0.1:${toString cfg.port}";
      extraConfig = "client_max_body_size 25m;";
    };
  };
}
