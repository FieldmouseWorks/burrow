# The fix plugin and its broker (crates/burrow-fix; Conary's
# docs/m6-plugins.md, F). Conary's flow hands a PR's findings, red checks
# and conflicts to the plugin, which forwards each to the broker, and the
# broker runs the fix as a proj job on the fixing role: proj keeps the
# slots, engines, sandboxes, the acting App and usage overflow. The broker
# runs as burrow.user, since proj's jobs are that user's, and holds no key;
# only the flow's controller may ask it (its socket's directory and its peer
# check). Off by default: Conary#314 turns the flow's plugins on.
#
# The same broker serves the stuck role's `job` (Conary's
# docs/m6-properties.md, SJ) through a plugin of its own, which names
# itself by the stuck role's engine: burrow.autoland.plugins.job, off by
# default, and named for `job` in the flow's configuration only when on.
# Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.fix;
  job = config.burrow.autoland.plugins.job;
  autoland = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (config.burrow.github) machineApp;
  flow = config.burrow.ledger.flow;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-fix;
  flowFiles = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-flow-files;
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/fix/broker.sock";

  configFile = pkgs.writeText "burrow-fix.json" (builtins.toJSON cfg.config);
  # Where the plugins read it: the machine's copy, not the store path,
  # since with the job kind on it names the flow's configs, which name the
  # plugins, as the judge's does.
  etcConfig = "burrow/autoland-fix-plugin.json";
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
        # The stuck role, when the broker serves the job kind.
        stuck = if job.enable then job.role else null;
        # The flow's configurations, whose review and flow authors are the
        # only ones a stuck-role job is shown the discussion of, and whose
        # job_login and flow authors the only ones a follow-up issue it's
        # handed may have been opened by.
        flow = if job.enable then autoland.flowConfigs else null;
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
        exec ${lib.getExe package} plugin /etc/${etcConfig}
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

  options.burrow.autoland.plugins.job = {
    enable = lib.mkEnableOption ''
      the stuck role's `job` plugin, which the fix broker serves: the flow
      hands a PR it can't settle to the stuck role as a proj job. Off until
      Conary's flow sends it (FP11); it needs burrow.ledger.controllerUser'';

    role = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9-]+";
      default = "stuck";
      description = ''
        The role a stuck-role job runs as, which picks its engine, model and
        effort (`proj settings role`). The plugin offers what that role's
        engine offers, as the fix plugin does the fixing role's.
      '';
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-fix-job-plugin" ''
        exec ${lib.getExe package} job /etc/${etcConfig}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for the job kind.";
    };

    choice = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      internal = true;
      default = "${flowFiles}/bin/burrow-flow-files choice ${config.burrow.settings.declared} ${job.role}";
      defaultText = lib.literalMD "the command that prints the stuck role's choice";
      description = ''
        The command that prints what the flow configuration's `execution`
        asks the job kind to run with: the stuck role's engine, model and
        effort as the settings source declares them.
      '';
    };
  };

  config = lib.mkIf (autoland.enable && (cfg.enable || job.enable)) {
    assertions = [{
      assertion = config.burrow.ledger.enable && flow.controller != null;
      message = "burrow.autoland.plugins.fix: only the flow's controller may ask the broker, so it needs burrow.ledger with its controllerUser.";
    } {
      assertion = job.enable -> machineApp != null;
      message = "burrow.autoland.plugins.job: a stuck-role job opens a PR's follow-up issue as the machinery's App, and the broker takes no issue anyone else opened for one, so it needs burrow.github.machineApp.";
    }];

    systemd.tmpfiles.rules = [ "d ${stateDir}/fix 0700 ${user} - -" ];
    environment.etc.${etcConfig}.source = configFile;

    systemd.services.burrow-fix = {
      description = "Conary's fix broker: the flow's fixes and stuck-role jobs, as proj jobs";
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
