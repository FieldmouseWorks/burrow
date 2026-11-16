# The judge plugin and its producer (crates/burrow-judge; Conary's
# docs/m6-plugins.md, J). Conary's flow asks the plugin to judge a PR's
# head, and the plugin forwards each request to the producer,
# autoland-judge: a user of its own, which alone gets the signing key and
# the seal key it shares with the flow, as systemd credentials, and which
# only the flow's controller may ask (its socket's directory and its peer
# check). It reads GitHub and posts its sealed comments and check runs as
# an App (burrow.github.apps).
#
# The judgment itself runs as a proj job on the judging role, and proj's
# jobs are burrow.user's, the user authoring sessions run as, so no key may
# be that user's: the runner, burrow-judge-runner, runs as burrow.user with
# no key, and only autoland-judge may ask it (its peer check). It briefs
# the job, waits on it through its record and stops it with its request.
#
# Off by default: Conary#314 turns the flow's plugins on, with the
# producer's key, the seal key and the producer's line in the machine's
# producers file. Not before burrow#1186: the job, its answers and the
# runner are burrow.user's, so an authoring session could answer in the
# judge's place, and the judging engine must first move out of its reach.
# Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.judge;
  autoland = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (config.burrow.github) machineApp;
  flow = config.burrow.ledger.flow;
  apps = config.burrow.github.apps;
  app = if cfg.app == null then null else apps.${cfg.app} or null;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-judge;
  producer = "autoland-judge";
  runnerUnit = "burrow-judge-runner";
  state = "${stateDir}/${producer}";
  runnerState = "${stateDir}/judge-runner";
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/judge/producer.sock";
  runnerSocket = "/run/${runnerUnit}/runner.sock";
  gh = config.burrow.tools.gh or pkgs.gh;
  # What prints a repo's token (its last argument) for Conary's reads: the
  # producer's App's, and the runner's as the machine's own jobs read GitHub.
  appToken = socketPath: pkgs.writeShellScript "burrow-judge-token" ''
    GH_TOKEN_SOCKET=${lib.escapeShellArg socketPath} exec ${config.burrow.tools.gh-token}/bin/gh-token get "$1"
  '';
  producerToken = if app == null then null else appToken app.socket;
  runnerToken = if machineApp == null
    then pkgs.writeShellScript "burrow-judge-runner-token" ''
      exec ${lib.getExe gh} auth token
    ''
    else appToken machineApp.socket;
  configFile = pkgs.writeText "burrow-judge.json" (builtins.toJSON cfg.config);
in
{
  options.burrow.autoland.plugins.judge = {
    enable = lib.mkEnableOption ''
      Conary's judge plugin and its producer, whose judgments run as proj
      jobs on the judging role and whose keys seal and sign each. Off until
      Conary#314 turns the flow's plugins on; it needs
      burrow.ledger.controllerUser, an App, a signing key and the flow's
      seal key'';

    producer = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9._/-]+";
      example = "remi/judge";
      description = ''
        The producer's name, as the machine's producers file
        (burrow.ledger's evidence.producers) names its public key and the
        check it's trusted for: `conary/judgment`.
      '';
    };

    role = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9-]+";
      default = "judge";
      description = ''
        The role a judge job runs as, which picks its engine, model and
        effort (`proj settings role`). The plugin offers what that role's
        engine offers, and Conary refuses an answer from any other engine:
        so the role names its model, and moves to no other engine.
      '';
    };

    keyFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/judge-producer-key";
      description = ''
        The producer's Ed25519 secret, its 32 bytes, readable by root: a
        secret, never in the store. The producer alone gets it, as its
        systemd credential.
      '';
    };

    sealFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/judge-seal-key";
      description = ''
        The flow's judge seal key (its `settings.judge_key`), as 64
        lowercase hex characters, readable by root: a secret, never in the
        store. The producer alone gets it, as its systemd credential.
      '';
    };

    app = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = config.burrow.github.machinery;
      defaultText = lib.literalExpression "config.burrow.github.machinery";
      description = ''
        The App, one of burrow.github.apps, the producer reads GitHub as and
        posts its sealed comments and check runs as: one the flow reads
        verdicts from (`settings.verdict_authors`). That App isn't the
        trust root: the seal and the signed statement are.
      '';
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      internal = true;
      # What crates/burrow-judge reads (its Config): the plugin, the
      # producer and the runner alike.
      default = {
        inherit socket;
        inherit (flow) controller;
        runner = runnerSocket;
        user = producer;
        inherit (cfg) producer role;
        credential = "judge-key";
        seal = "judge-seal";
        flow = autoland.flowConfigs;
        settings = config.burrow.settings.effective;
        proj = [ "${config.burrow.tools.proj}/bin/proj" ];
        jobs = config.burrow.proj.jobsDir;
        # The machine's own work, which acts on GitHub as the machinery's App.
        app = if machineApp == null then null else machineApp.name;
        timeout = "${toString autoland.jobTimeoutMinutes}m";
        idle = "${toString autoland.jobIdleMinutes}m";
        owner = config.burrow.owner;
        api = "https://api.github.com";
        token = if producerToken == null then null else "${producerToken}";
        runner_token = "${runnerToken}";
        gh = if app == null then null else "${app.gh}/bin/gh";
        inherit state;
        runner_state = runnerState;
      };
      description = "What the plugin, the producer and the runner read.";
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-judge-plugin" ''
        exec ${lib.getExe package} plugin ${configFile}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for judgments.";
    };
  };

  config = lib.mkIf (autoland.enable && cfg.enable) {
    assertions = [
      {
        assertion = config.burrow.ledger.enable && flow.controller != null;
        message = "burrow.autoland.plugins.judge: only the flow's controller may ask the producer, so it needs burrow.ledger with its controllerUser.";
      }
      {
        assertion = app != null && lib.elem producer app.users;
        message = "burrow.autoland.plugins.judge: the producer acts on GitHub as an App, so it needs burrow.autoland.plugins.judge.app, one of burrow.github.apps, with ${producer} among its users.";
      }
    ];

    users.users.${producer} = {
      isSystemUser = true;
      group = producer;
    };
    users.groups.${producer} = { };

    # Beside the App's default askers, not instead of them.
    burrow.github.apps = lib.mkIf (cfg.app != null) {
      ${cfg.app}.users = lib.mkOptionDefault [ producer ];
    };

    systemd.tmpfiles.settings.burrow-judge = {
      ${state}.d = { mode = "0700"; user = producer; group = producer; };
      ${runnerState}.d = { mode = "0700"; inherit user; group = "-"; };
    };

    systemd.services.${producer} = {
      description = "Conary's judge producer: the flow's judgments, sealed and signed";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" "${runnerUnit}.service" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (app != null) app.env;
      # With no controller, the assertion above says why, rather than
      # flow.producer's throw.
      serviceConfig = lib.optionalAttrs (flow.controller != null) (flow.producer "judge") // {
        User = producer;
        Group = producer;
        ExecStart = "${lib.getExe package} producer ${configFile}";
        LoadCredential = [ "judge-key:${cfg.keyFile}" "judge-seal:${cfg.sealFile}" ];
        UMask = "0077";
        # A restart drops its requests, whose plugins see their connections
        # close; what a crash left is finished at its start.
        Restart = "on-failure";
      };
    };

    systemd.services.${runnerUnit} = {
      description = "Conary's judge runner: the producer's judgments, as proj jobs";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        User = user;
        ExecStart = "${lib.getExe package} runner ${configFile}";
        # Anyone may reach the socket: its peer check lets autoland-judge
        # alone ask.
        RuntimeDirectory = runnerUnit;
        RuntimeDirectoryMode = "0755";
        # A restart stops the jobs whose requests it lost, and the
        # producer sees its connections close.
        Restart = "on-failure";
      };
    };
  };
}
