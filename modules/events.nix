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

  # Deliveries come in only where GitHub can reach the machine.
  hooked = cfg.host != null;
  configFile = pkgs.writeText "burrow-events.json" (builtins.toJSON {
    inherit (cfg) kickPort;
    port = if hooked then cfg.port else null;
    handlers = lib.mapAttrs (_: h: { inherit (h) events unit wake ownStatuses perRepo repos pushes branches; }) cfg.handlers;
  });
  src = pkgs.runCommand "burrow-events" { nativeBuildInputs = [ pkgs.python3.pkgs.flake8 ]; } ''
    mkdir $out
    cp ${./events/events.py} $out/events.py
    flake8 --ignore E501,W503 $out
  '';
  units = lib.mapAttrsToList (_: h: h.unit) cfg.handlers;
  # Each one best-effort: burrow-events down (restarting, say) runs every
  # full pass when it starts anyway.
  kickAll = lib.concatMapStrings (name: ''
    curl -fsS -m 10 -d "" ${cfg.kick}/${name} >/dev/null || echo "couldn't kick ${name}" >&2
  '') (lib.attrNames cfg.handlers);
in
{
  options.burrow.events = {
    enable = mkOption {
      type = types.bool;
      # A module that registers a handler has no other way to run it.
      default = cfg.handlers != { };
      defaultText = lib.literalExpression "config.burrow.events.handlers != { }";
      description = ''
        Whether to run burrow-events: GitHub's webhook deliveries, local
        kicks and the passes' own wake-ups start each handler's pass. nginx
        on this machine has to serve `host` over HTTPS. On whenever a module
        registers a handler, since that's the only thing that runs it.
      '';
    };

    host = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "example.org";
      description = ''
        The nginx virtual host GitHub reaches this machine at: the hooks' URL
        is https://HOST/PATH. Without one (and `secretFile`), no deliveries
        come in, and passes start only on kicks, wake-ups and the catch-up.
        The steward (burrow.autoland) needs both.
      '';
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
      type = types.nullOr types.str;
      default = null;
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
        the one repo its instance names (`%I` is OWNER/REPO), unless `perRepo`
        is off.
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
          perRepo = mkOption {
            type = types.bool;
            default = true;
            description = ''
              Whether the handler has a pass over one repo (UNIT@.service). Off,
              a repo's event starts its full pass, and UNIT.service is all
              there is.
            '';
          };
          repos = mkOption {
            type = types.nullOr (types.listOf types.str);
            default = null;
            example = [ "sam/homelab" ];
            description = ''
              The repos (OWNER/NAME, case-blind) whose deliveries and kicks
              start the handler's passes. null: every repo; [ ]: none, so only
              full passes (a kick, the catch-up).
            '';
          };
          pushes = mkOption {
            type = types.enum [ "any" "default" ];
            default = "any";
            description = ''
              Which pushes count, if `events` has push: to any branch, or only
              to the repo's default branch (a merge, say).
            '';
          };
          branches = mkOption {
            type = types.nullOr (types.listOf types.str);
            default = null;
            example = [ "renovate/*" ];
            description = ''
              For the events about a branch other than a push (a pull
              request's head, the commit a check suite or status is for): only
              those about one of these branches count (globs). An event about
              no branch (an issue, a comment) always does. null: every branch.
            '';
          };
          ownStatuses = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = ''
              The commit status contexts the handler's passes post themselves
              (the steward's `review-settled`): a `status` delivery for one
              is no news to it, so it starts none of its passes. Other
              handlers still get it.
            '';
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

    url = mkOption {
      type = types.nullOr types.str;
      readOnly = true;
      default = if hooked then "https://${cfg.host}${cfg.path}" else null;
      defaultText = lib.literalExpression ''"https://''${host}''${path}"'';
      description = "The hooks' URL, if the machine takes deliveries.";
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
    assertions = [{
      assertion = (cfg.host == null) == (cfg.secretFile == null);
      message = "burrow.events: deliveries need both host and secretFile (or neither, for none).";
    }];

    # The repos' hooks are the steward's to keep (autoland/repos.py).
    warnings = lib.optional (!(lib.attrByPath [ "burrow" "autoland" "enable" ] false config)) ''
      burrow.events is on without burrow.autoland, which keeps each repo's
      webhook: GitHub's events won't reach this machine, so its passes come
      from local kicks, their own wake-ups and the hourly catch-up.
    '';

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
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
      } // lib.optionalAttrs hooked {
        LoadCredential = "webhook:${cfg.secretFile}";
      };
    };

    # A new secret (secretFile written, or replaced as sops does) restarts the
    # service, which reads it only when it starts. Its catch-up full passes
    # then give every repo's hook the new one: so the restart waits for any
    # full pass already running, which read the old one and would start
    # nothing new if asked again while it runs.
    systemd.paths.burrow-events-secret = lib.mkIf hooked {
      wantedBy = [ "multi-user.target" ];
      pathConfig.PathChanged = cfg.secretFile;
    };
    systemd.services.burrow-events-secret = lib.mkIf hooked {
      description = "burrow-events: restart for a new webhook secret";
      after = map (u: "${u}.service") units;
      path = [ config.systemd.package ];
      script = "systemctl try-restart burrow-events.service";
      serviceConfig.Type = "oneshot";
    };

    # The only timer: a full pass of every handler each hour, for what no
    # event reports. A delivery GitHub couldn't make (this machine down, or
    # nginx) is never sent again, and a repo made on a user account sends
    # nothing before its first pass gives it a hook. A pass that has nothing
    # to do asks GitHub for little: one listing, a few points. With no
    # handlers there's nothing to pass over.
    systemd.services.burrow-events-catch-up = lib.mkIf (cfg.handlers != { }) {
      description = "burrow-events: a full pass of every handler, for what no event reported";
      path = [ pkgs.curl ];
      script = kickAll;
      serviceConfig = {
        Type = "oneshot";
        User = "burrow-events";
      };
    };
    systemd.timers.burrow-events-catch-up = lib.mkIf (cfg.handlers != { }) {
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

    services.nginx.virtualHosts = lib.mkIf hooked {
      ${cfg.host}.locations.${cfg.path} = {
        proxyPass = "http://127.0.0.1:${toString cfg.port}";
        extraConfig = "client_max_body_size 25m;";
      };
    };
  };
}
