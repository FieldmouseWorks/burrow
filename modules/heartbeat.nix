# heartbeat: something off this machine watches it. Every few minutes the
# machine tells a Healthchecks check (healthchecks.io, or an instance of your
# own) how it is, through its ping API (crates/burrow-heartbeat). When the pings
# stop, because the machine is down, hung or cut off, or when they say a health
# check keeps failing, Healthchecks tells the owner by whatever its check is
# wired to (email, a push to their phone), with nothing here having to be
# alive for it.
#
# The checks are HOST-health's (burrow.updates.health; without burrow.updates
# there are only those other modules add, burrow.ops' watchdog say, and with
# none the beat only says the machine is up). Its timer is on purpose: silence
# is the signal, and nothing sends an event when a machine dies.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.heartbeat;
  inherit (lib) mkOption types;
  name = "${config.burrow.prefix}-heartbeat";

  # What crates/burrow-heartbeat reads (its Config).
  heartbeatConfig = pkgs.writeText "${name}.json" (builtins.toJSON {
    inherit name;
    inherit (cfg) url check failAfter skip;
    health = lib.getExe config.burrow.tools.health;
  });

  heartbeat = pkgs.runCommand name {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = name;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-heartbeat}/bin/burrow-heartbeat \
      $out/bin/${name} --add-flags ${heartbeatConfig}
  '';

  # One site's watch: a success ping to its check while it answers, nothing
  # when it doesn't (its silence is the alert). Neither a site down nor a
  # lost ping fails the unit: a failed unit would only be noise in HOST-health.
  siteWatch = slug: url: pkgs.writeShellScript "${name}-site-${slug}" ''
    key=$(tr -d '[:space:]' <"$CREDENTIALS_DIRECTORY/ping-key")
    if ! ${lib.getExe pkgs.curl} -fsS -o /dev/null --max-time 20 ${lib.escapeShellArg url} 2>/dev/null; then
      echo ${lib.escapeShellArg "${slug}: ${url} didn't answer, so no ping"}
    elif ${lib.getExe pkgs.curl} -fsS -o /dev/null --max-time 10 --retry 2 "${cfg.url}/$key/${slug}" 2>/dev/null; then
      echo ${lib.escapeShellArg "${slug}: ${url} answers"}
    else
      echo ${lib.escapeShellArg "${slug}: ${url} answers, but the ping to ${slug} didn't land"} >&2
    fi
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/updates"; imports = [ (import ./updates.nix inputs) ]; }
  ];

  options.burrow.heartbeat = {
    enable = lib.mkEnableOption "heartbeat, pings to an outside Healthchecks check that alerts when they stop";

    url = mkOption {
      type = types.str;
      default = "https://hc-ping.com";
      description = "Where the pings go: a Healthchecks instance's ping endpoint.";
    };

    pingKeyFile = mkOption {
      type = types.str;
      default = "/run/secrets/heartbeat";
      description = ''
        A file holding the ping key of the Healthchecks project the check is
        in (its Settings, "Ping key"). Whoever has it can report for this
        machine, so keep it root's: the unit reads it as a credential.
      '';
    };

    check = mkOption {
      type = types.str;
      default = config.networking.hostName;
      defaultText = lib.literalExpression "config.networking.hostName";
      description = ''
        The check's slug in that project: the pings go to URL/KEY/SLUG. Make it
        by hand, with a period of `every` and a grace that covers a reboot and
        a late beat (10 minutes on the defaults), and wire it to
        whatever should reach the owner. A ping to a check that isn't there
        is only logged here; it shows there as the check going quiet.
      '';
    };

    every = mkOption {
      type = types.str;
      default = "5min";
      description = "How often the machine beats (a systemd time span).";
    };

    failAfter = mkOption {
      type = types.ints.positive;
      default = 2;
      description = ''
        How many beats running a health check has to fail before the beat
        says so, which alerts at once. Until then the beat is still a success
        that names it: a switch restarting a service shouldn't wake anyone.
      '';
    };

    skip = mkOption {
      type = types.listOf types.str;
      default = [ "github" ];
      description = ''
        Health checks that don't count here. GitHub down isn't this machine
        down, and when the machine's own network is, the pings stop anyway.
      '';
    };

    sites = mkOption {
      type = types.attrsOf types.str;
      default = { };
      example = { shop = "https://shop.example.org/health"; };
      description = ''
        Sites elsewhere this machine watches, by the slug of their own check
        in the same Healthchecks project. Every `every` the site's URL is
        fetched, and while it answers 2xx its check gets a success ping. When
        it stops answering, the pings stop and Healthchecks alerts after the
        check's period and grace, so a blip shorter than the grace wakes no
        one. Make each check by hand, as for `check`. The pings also stop when
        this machine is down, which its own check says too.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [ {
    systemd.services.${name} = {
      description = "Tell the outside watcher how this machine is";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = lib.getExe heartbeat;
        LoadCredential = "ping-key:${cfg.pingKeyFile}";
        # The streaks of failed beats, kept from one beat to the next.
        RuntimeDirectory = name;
        RuntimeDirectoryPreserve = true;
      };
    };

    burrow.claude.machineMd = lib.mkOrder 1310 ''
      ${name} pings a Healthchecks check (${cfg.check}) every ${cfg.every}
      with the health checks' verdict, and Healthchecks tells ${config.burrow.owner} when the
      pings stop or say a check keeps failing. So taking ${config.networking.hostName} down for
      longer than the check's grace (a rescue boot, say) alerts ${config.burrow.owner}: tell
      them first.
    '';
    systemd.timers.${name} = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "1min";
        OnUnitActiveSec = cfg.every;
        AccuracySec = "10s";
      };
    };
  } {
    systemd.services = lib.mapAttrs' (slug: url: lib.nameValuePair "${name}-site-${slug}" {
      description = "Tell the outside watcher that ${url} answers";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = siteWatch slug url;
        LoadCredential = "ping-key:${cfg.pingKeyFile}";
      };
    }) cfg.sites;
    systemd.timers = lib.mapAttrs' (slug: _: lib.nameValuePair "${name}-site-${slug}" {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "1min";
        OnUnitActiveSec = cfg.every;
        AccuracySec = "10s";
      };
    }) cfg.sites;
    burrow.claude.machineMd = lib.mkIf (cfg.sites != { }) (lib.mkOrder 1311 ''
      ${name} also watches ${lib.concatStringsSep ", " (lib.mapAttrsToList (slug: url: "${url} (check ${slug})") cfg.sites)}:
      while a site answers, its check gets a ping every ${cfg.every}, and
      when it stops, Healthchecks tells ${config.burrow.owner}.
    '');
  } ]);
}
