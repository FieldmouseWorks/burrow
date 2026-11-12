# Admission policy: the flake seeds one document, the independent door writes
# one runtime overlay. Every consumer uses burrow-settings to read it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.settings;
  inherit (config.burrow) user stateDir;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-settings;
  settings = {
    overlay = "${stateDir}/settings/overlay.json";
    inherit (cfg) allowed;
    # A release through the door is ordered with proj's queue births by it.
    usageLock = "${lib.attrByPath [ "burrow" "proj" "jobsDir" ] "${stateDir}/jobs" config}/.usage.lock";
    pipelineRepos = lib.unique (lib.attrByPath [ "burrow" "autoland" "pipelineRepos" ] [ ] config ++ lib.attrByPath [ "burrow" "autoland" "leadRepos" ] [ ] config);
  };
  tracked = pkgs.writeText "burrow-settings.json" (builtins.toJSON settings);
  command = pkgs.writeShellScriptBin "settings" ''
    exec ${package}/bin/burrow-settings ${tracked} "$@"
  '';
  door = pkgs.writeText "settings-door.json" (builtins.toJSON {
    inherit settings;
    inherit (cfg.door) repo label authors;
    state = "${stateDir}/settings-door";
  });
in {
  imports = [ (import ./common.nix inputs) { key = "burrow/github"; imports = [ (import ./github.nix inputs) ]; } ];
  options.burrow.settings = {
    allowed = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Repositories whose automated work may move. Unlisted and new repositories are off. Unlisted repositories can still be triaged; explicit runtime pauses vacate every role. Runtime changes use settings allow/pause.";
    };
    effective = lib.mkOption { type = lib.types.attrs; readOnly = true; internal = true; };
    file = lib.mkOption { type = lib.types.path; readOnly = true; internal = true; };
    command = lib.mkOption { type = lib.types.str; readOnly = true; internal = true; };
    door = {
      enable = lib.mkEnableOption "independent settings issue door";
      gh = lib.mkOption { type = lib.types.package; default = config.burrow.github.machineGh; internal = true; };
      repo = lib.mkOption { type = lib.types.str; default = ""; description = "Repository accepting settings requests."; };
      label = lib.mkOption { type = lib.types.str; default = "settings"; };
      authors = lib.mkOption { type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; }); default = [ ]; };
    };
  };
  config = lib.mkMerge [ {
    burrow.settings = { effective = settings; file = tracked; command = "${package}/bin/burrow-settings"; };
    environment.systemPackages = [ command ];
    systemd.tmpfiles.rules = [ "d ${stateDir}/settings 0755 ${user} - -" ];
  } (lib.mkIf cfg.door.enable {
    assertions = [ { assertion = cfg.door.authors != [ ] && cfg.door.repo != ""; message = "The settings door requires a repository and authorized authors."; } ];
    systemd.tmpfiles.rules = [ "d ${stateDir}/settings-door 0700 ${user} - -" ];
    # This timer is independent of the pipeline and its event dispatcher.
    systemd.timers.burrow-settings-door = {
      wantedBy = [ "timers.target" ];
      timerConfig = { OnBootSec = "10s"; OnUnitInactiveSec = "30s"; };
    };
    systemd.services.burrow-settings-door = {
      description = "Change operational settings without the pipeline";
      path = [ cfg.door.gh ];
      environment.GH_NO_UPDATE_NOTIFIER = "1";
      environment.BURROW_DOOR_WAKE = "${stateDir}/settings-door/wake.json";
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${package}/bin/burrow-settings ${door} door";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "full";
        TimeoutStartSec = "1min";
      };
    };
  }) ];
}
