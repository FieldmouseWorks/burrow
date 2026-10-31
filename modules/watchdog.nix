# watchdog: work that has stopped moving gets a look before anyone has to
# notice it (crates/burrow-watchdog). Each source (the steward's open PRs, say)
# keeps a watch file of what it waits on and by when that should have moved
# on. Past that, the watchdog kicks the item's pass, then hands it to a
# Claude job, then to the steward's stuck issue, whose own job (proj run
# --unstick) takes it from there. The owner hears only what that can't take:
# with no steward, when the steward is what's stuck, or when that job is.
# It's a burrow-events handler: a source kicks it when its items change, its
# job's end kicks it, and it books its own next pass at the next deadline.
# No timer, and no GitHub calls of its own.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.watchdog;
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  inherit (cfg) dir;
  autoland = config.burrow.autoland.enable or false;

  configFile = pkgs.writeText "burrow-watchdog.json" (builtins.toJSON {
    stateDir = dir;
    sources = lib.mapAttrs (_: s: {
      inherit (s) file about look kick;
      stale = if s.staleMinutes == null then null else s.staleMinutes * 60;
    }) cfg.sources;
    proj = "${config.burrow.tools.proj}/bin/proj";
    inherit (cfg) project;
    jobTimeout = cfg.jobTimeoutMinutes;
    jobIdle = cfg.jobIdleMinutes;
    # It's the machine's own work, so its job acts on GitHub as the
    # machinery's App (proj run --app); null for none.
    app = if config.burrow.github.machineApp == null then null else config.burrow.github.machineApp.name;
    inherit (config.burrow.events) kick;
    escalateAfter = cfg.escalateAfterMinutes * 60;
    # The stuck issue is the steward's to publish (autoland/repos.py), on the
    # pass this kicks.
    stewardKick = if autoland then "autoland" else null;
    labels = if autoland then { inherit (config.burrow.autoland.labels) owner hold; } else null;
    notify = lib.optionalString (config.burrow.tools ? notify) (lib.getExe config.burrow.tools.notify);
    inherit (config.burrow) owner;
    host = config.networking.hostName;
  });
  # The command, with this machine's config, so its jobs can snooze and
  # anyone can ask for its status from any shell.
  watchdog = pkgs.writeShellScriptBin "burrow-watchdog" ''
    export WATCHDOG_CONFIG=''${WATCHDOG_CONFIG:-${configFile}}
    exec ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/burrow-watchdog "$@"
  '';
in
{
  imports = [
    (import ./common.nix inputs)
    { key = "burrow/proj"; imports = [ (import ./proj.nix inputs) ]; }
    { key = "burrow/events"; imports = [ (import ./events.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./watchdog/sources.nix ]; }
  ];

  options.burrow.watchdog = {
    enable = lib.mkEnableOption ''
      the stall watchdog: what its sources wait on past its deadline is
      kicked, then handed to a Claude job, then to the steward's stuck issue
      and its job (the owner hears only when there's no steward, or the
      steward or that job is what's stuck)
    '';

    project = mkOption {
      type = types.str;
      default = baseNameOf config.burrow.flake;
      defaultText = lib.literalExpression "baseNameOf config.burrow.flake";
      description = ''
        The project (a folder in burrow.projectsDir) its Claude jobs run in:
        the machine's own flake by default, where the journals, the
        services and the operator's notes are.
      '';
    };

    escalateAfterMinutes = mkOption {
      type = types.ints.positive;
      default = 120;
      description = ''
        How long its Claude job has with a stall before it goes to the
        steward's stuck issue (sooner once the job has ended with it still
        stuck, or if the job hasn't started in 15 minutes).
      '';
    };

    jobTimeoutMinutes = mkOption {
      type = types.ints.positive;
      default = 60;
      description = "How long its Claude job's turn may run (proj run --timeout).";
    };

    jobIdleMinutes = mkOption {
      type = types.ints.positive;
      default = 20;
      description = "How long its Claude job's turn may say nothing before it's stopped as stuck (proj run --idle).";
    };
  };

  config = lib.mkIf cfg.enable {
    burrow.proj.enable = true;
    environment.systemPackages = [ watchdog ];

    burrow.events.handlers.watchdog = {
      # Nothing on GitHub: its sources' kicks, its job's end and its own
      # wake-ups start its passes, which read only this machine.
      events = [ ];
      repos = [ ];
      perRepo = false;
      unit = "burrow-watchdog";
      wake = "${dir}/wake.json";
    };
    systemd.services.burrow-watchdog = {
      description = "burrow-watchdog: take what has stopped moving a step further";
      path = [ config.burrow.tools.proj ];
      serviceConfig = {
        Type = "oneshot";
        # proj's jobs are the user's, and so are its sources' files.
        User = user;
        ExecStart = "${watchdog}/bin/burrow-watchdog";
        TimeoutStartSec = "5min";
      };
    };
    systemd.tmpfiles.settings.burrow.${dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };

    burrow.claude.machineMd = lib.mkOrder 1060 ''
      ## Stalls

      `burrow-watchdog` watches what the pipeline waits on (${lib.concatStringsSep ", " (lib.attrNames cfg.sources)}) and, past
      each wait's deadline, kicks it, then hands it to a Claude job, then to the stuck issue, whose
      own job takes it from there; ${config.burrow.owner} hears only when the steward or that job is
      what's stuck. `burrow-watchdog status` shows what it watches and what's stalled.
    '';
  };
}
