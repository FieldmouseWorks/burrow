# Shared Actions degradation source. Each importing machine chooses its
# repositories and workflow files; nothing here names a particular machine.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.watchdog.degradations;
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  dir = "${config.burrow.stateDir}/degradations";
  file = "${dir}/watch.json";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-watchdog;
  settings = pkgs.writeText "burrow-degradations.json" (builtins.toJSON {
    inherit file;
    inherit (cfg) repos prThreshold authors;
    kick = "${config.burrow.events.kick}/watchdog";
  });
in {
  imports = [
    { key = "burrow/github"; imports = [ (import ../github.nix inputs) ]; }
  ];
  options.burrow.watchdog.degradations = {
    enable = lib.mkEnableOption "watching repeated Actions degradations through the stall watchdog";
    repos = mkOption {
      type = types.attrsOf (types.listOf (types.strMatching "[A-Za-z0-9_.-]+"));
      default = { };
      example = { "sam/homelab" = [ "flake.yml" ]; };
      description = "Repositories and workflow filenames to read. The GitHub login needs Actions: read. Only completed runs count.";
    };
    authors = mkOption {
      type = types.listOf types.str;
      default = config.burrow.autoland.authors or [ ];
      defaultText = lib.literalExpression "config.burrow.autoland.authors";
      description = "Whose pull-request runs may supply degradation keys and links to the fix job. A run counts only when its original actor (a rerun by someone else doesn't change it) is listed and its head is in the same repository.";
    };
    prThreshold = mkOption {
      type = types.ints.positive;
      default = 3;
      description = "Distinct PR runs in 24 hours with the same degradation key that constitute a stall. Two consecutive default-branch push runs also constitute a stall.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [ {
      assertion = config.burrow.watchdog.enable;
      message = "watchdog.degradations requires watchdog.enable";
    } {
      assertion = cfg.authors != [ ];
      message = "burrow.watchdog.degradations.authors: name whose PR runs may be read.";
    } ];
    burrow.events.handlers.degradations = {
      events = [ "workflow_run" ];
      repos = lib.attrNames cfg.repos;
      perRepo = false;
      unit = "burrow-degradations";
      gap = 120;
    };
    systemd.services.burrow-degradations = {
      description = "Read repeated Actions degradations into the stall watchdog";
      restartIfChanged = false;
      path = [ config.burrow.github.machineGh ];
      serviceConfig = {
        Type = "oneshot";
        User = user;
        Environment = "DEGRADATIONS_CONFIG=${settings}";
        ExecStart = "${package}/bin/burrow-degradations";
        TimeoutStartSec = "5min";
      };
    };
    systemd.tmpfiles.settings.burrow.${dir}.d = {
      mode = "0755"; inherit user; group = config.users.users.${user}.group;
    };
    burrow.watchdog.sources.degradations = {
      inherit file;
      about = "The Actions degradation reader";
      look = "Read journalctl -u burrow-degradations and the linked run's degraded/KEY step and summary. Fix the workflow's cause; subsequent healthy default-branch runs or expiry of the PR window clear it. A kick rereads Actions without rerunning builds.";
      kick = "degradations";
      staleMinutes = 120;
    };
  };
}
