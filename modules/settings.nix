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
    legacyRoles = if config.burrow.proj.enable or false then "${config.users.users.${user}.home}/.config/proj" else null;
    legacyEnginesLock = if (config.burrow.proj.enable or false) && (config.burrow.proj.engines.door.enable or false)
      then "${stateDir}/proj-engines/${lib.toLower (builtins.replaceStrings [ "/" ] [ "--" ] config.burrow.proj.engines.door.repo)}/lock" else null;
    # Who lands each repo (autoland/landing.nix): the record, wherever the
    # steward runs, and the repos conaryd may be given.
    landingRecord = if config.burrow.autoland.enable or false then config.burrow.landing.record else null;
    conaryd = lib.attrByPath [ "burrow" "landing" "conaryd" ] [ ] config;
    pipelineRepos = lib.unique (lib.attrByPath [ "burrow" "autoland" "pipelineRepos" ] [ ] config ++ lib.attrByPath [ "burrow" "autoland" "leadRepos" ] [ ] config);
    engines = lib.unique (
      [ "claude" "claude-api" ]
      ++ lib.attrNames (
        lib.filterAttrs (_: e: (e.plugin or null) != null) (config.burrow.agents.engines or { })
      )
      ++ lib.attrNames (config.burrow.proj.plugins or { })
    );
    owners = cfg.owners;
    pluginsDir = cfg.plugins.dir;
    pluginFallback = cfg.plugins.fallback;
    pluginSandbox = "${cfg.plugins.sandbox}/bin/bwrap";
    pluginCurl = "${cfg.plugins.curl}/bin/curl";
    pluginCheckTimeoutSeconds = cfg.plugins.checkTimeoutSeconds;
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
    owners = lib.mkOption {
      type = lib.types.listOf (import ../lib/github-author.nix { inherit lib; });
      default = cfg.door.authors;
      defaultText = lib.literalExpression "config.burrow.settings.door.authors";
      description = "Authorized owner identities (by positive numeric GitHub account ID) allowed to install, update, or remove pinned engine plugins through the settings write door. Empty fails closed.";
    };
    plugins = {
      dir = lib.mkOption {
        type = lib.types.str;
        default = "${stateDir}/settings/plugins";
        defaultText = lib.literalExpression ''"''${config.burrow.stateDir}/settings/plugins"'';
        description = "Host-owned directory storing verified release engine plugin artifacts and staging files.";
      };
      fallback = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "claude";
        description = "Declared default fallback engine for roles affected when an installed engine plugin is removed; null refuses removal unless a valid --fallback engine is supplied.";
      };
      sandbox = lib.mkOption {
        type = lib.types.package;
        default = pkgs.bubblewrap;
        defaultText = lib.literalExpression "pkgs.bubblewrap";
        internal = true;
        description = "Sandbox runner (bubblewrap) used to confine cn plugin check before activating a plugin.";
      };
      curl = lib.mkOption {
        type = lib.types.package;
        default = pkgs.curl;
        defaultText = lib.literalExpression "pkgs.curl";
        internal = true;
        description = "HTTP client used to fetch pinned HTTPS release plugin artifacts before hash verification.";
      };
      checkTimeoutSeconds = lib.mkOption {
        type = lib.types.ints.positive;
        default = 30;
        description = "Per-invocation timeout in seconds for sandboxed cn plugin check before activation.";
      };
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
    systemd.tmpfiles.rules = [
      "d ${stateDir}/settings 0755 ${user} - -"
      "d ${cfg.plugins.dir} 0755 ${user} - -"
    ];
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
      restartIfChanged = false;
      # Oneshot with no Restart= and a 30s timer: don't let systemd's default
      # 5-starts-in-10s limit refuse repeated manual or test invocations.
      startLimitIntervalSec = 0;
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
        TimeoutStartSec = "3min";
      };
    };
  }) ];
}
