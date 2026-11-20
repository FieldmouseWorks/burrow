# The fix plugin and its broker (crates/burrow-fix; Conary's
# docs/m6-plugins.md, F). Conary's flow hands a PR's findings, red checks
# and conflicts to the plugin, which forwards each to the broker, and the
# broker runs the fix as a proj job on the fixing role: proj keeps the
# slots, engines, sandboxes, the acting App and usage overflow. The broker
# runs as burrow.user, since proj's jobs are that user's, and holds no key;
# only the flow's controller may ask it (its socket's directory and its peer
# check). Off by default: Conary#314 turns the flow's plugins on.
# Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.fix;
  autoland = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (config.burrow.github) machineApp;
  flow = config.burrow.ledger.flow;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-fix;
  flowFiles = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-flow-files;
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/fix/broker.sock";

  configFile = pkgs.writeText "burrow-fix.json" (builtins.toJSON cfg.config);
in
{
  options.burrow.autoland.plugins.fix = {
    enable = lib.mkEnableOption ''
      Conary's fix plugin and its broker, which run the flow's fixes as proj
      jobs. Off until Conary#314 turns the flow's plugins on; it needs
      burrow.ledger.controllerUser'';

    role = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9-]+";
      default = "fix";
      description = ''
        The role a fix job runs as, which picks its engine, model and effort
        (`proj settings role`). The plugin offers what that role's engine
        offers, and Conary refuses an answer from any other engine: so the
        role names its model, and moves to no other engine.
      '';
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      internal = true;
      # What crates/burrow-fix reads (its Config), the plugin and the broker
      # alike.
      default = {
        inherit socket;
        inherit (flow) controller;
        # Where the fixing role's route and its engine's offers are kept: the
        # plugin describes itself by them.
        settings = config.burrow.settings.effective;
        proj = [ "${config.burrow.tools.proj}/bin/proj" ];
        jobs = config.burrow.proj.jobsDir;
        inherit (cfg) role;
        # The machine's own work, which acts on GitHub as the machinery's App.
        app = if machineApp == null then null else machineApp.name;
        timeout = "${toString autoland.jobTimeoutMinutes}m";
        idle = "${toString autoland.jobIdleMinutes}m";
        gh = lib.getExe (config.burrow.tools.gh or pkgs.gh);
        state = "${stateDir}/fix";
      };
      description = "What the plugin and the broker read.";
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-fix-plugin" ''
        exec ${lib.getExe package} plugin ${configFile}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for fixes.";
    };

    choice = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      internal = true;
      default = "${flowFiles}/bin/burrow-flow-files choice ${config.burrow.settings.declared} ${cfg.role}";
      defaultText = lib.literalMD "the command that prints the fixing role's choice";
      description = ''
        The command that prints what the flow configuration's `execution`
        asks fixes to run with: the fixing role's engine,
        model and effort as the settings source declares them.
      '';
    };
  };

  config = lib.mkIf (autoland.enable && cfg.enable) {
    assertions = [{
      assertion = config.burrow.ledger.enable && flow.controller != null;
      message = "burrow.autoland.plugins.fix: only the flow's controller may ask the broker, so it needs burrow.ledger with its controllerUser.";
    }];

    systemd.tmpfiles.rules = [ "d ${stateDir}/fix 0700 ${user} - -" ];

    systemd.services.burrow-fix = {
      description = "Conary's fix broker: the flow's fixes, as proj jobs";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      # With no controller, the assertion above says why, rather than
      # flow.producer's throw.
      serviceConfig = lib.optionalAttrs (flow.controller != null) (flow.producer "fix") // {
        User = user;
        ExecStart = "${lib.getExe package} broker ${configFile}";
        # A restart stops the jobs whose requests it lost, and their plugins
        # see their connections close.
        Restart = "on-failure";
      };
    };
  };
}
