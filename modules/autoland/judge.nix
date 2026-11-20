# The judge plugin and its producer (crates/burrow-judge; Conary's
# docs/m6-plugins.md, J). Conary's flow asks the plugin to judge a PR's
# head, and the plugin forwards each request to the producer,
# autoland-judge: a user of its own, which alone gets the signing key and
# the seal key it shares with the flow, as systemd credentials, and which
# only the flow's controller may ask (its socket's directory and its peer
# check). It reads GitHub and posts its sealed comments and check runs as
# an App (burrow.github.apps).
#
# Each judgment runs in a look of its own, a unit only autoland-judge may
# start and stop (polkit), as a user systemd makes for it and takes back
# after (DynamicUser): the judging engine, Claude Code, in a fresh session,
# reading a checkout of the head the producer fetched itself, which is the
# only part of the producer's state it sees. Its login is its own systemd
# credential (claudeTokenFile), never burrow.user's, and it has no network
# but a gate the producer opens for it to Anthropic's API. Nothing of a
# look is burrow.user's, the user authoring sessions run as, so none of
# them can answer in the judge's place (burrow#1186).
#
# Off by default: Conary#314 turns the flow's plugins on, with the
# producer's key, the seal key, the look's login and the producer's line in
# the machine's producers file. Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.judge;
  autoland = config.burrow.autoland;
  inherit (config.burrow) stateDir;
  flow = config.burrow.ledger.flow;
  apps = config.burrow.github.apps;
  app = if cfg.app == null then null else apps.${cfg.app} or null;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-judge;
  flowFiles = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-flow-files;
  producer = "autoland-judge";
  look = "autoland-judge-look";
  state = "${stateDir}/${producer}";
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/judge/producer.sock";
  # What prints a repo's token (its last argument) for Conary's reads, as
  # the producer's App.
  producerToken = if app == null then null else pkgs.writeShellScript "burrow-judge-token" ''
    GH_TOKEN_SOCKET=${lib.escapeShellArg app.socket} exec ${config.burrow.tools.gh-token}/bin/gh-token get "$1"
  '';
  configFile = pkgs.writeText "burrow-judge.json" (builtins.toJSON cfg.config);
  # Where the plugin reads its config: not the store path itself, since the
  # config names the flow's configs (`flow`), and those name the plugin.
  etcConfig = "burrow/autoland-judge-plugin.json";
in
{
  options.burrow.autoland.plugins.judge = {
    enable = lib.mkEnableOption ''
      Conary's judge plugin and its producer, whose judgments each run in
      a look of the producer's own, on the judging role's engine, out of
      authoring sessions' reach, and whose keys seal and sign each. Off
      until Conary#314 turns the flow's plugins on; it needs
      burrow.ledger.controllerUser, an App, a signing key, the flow's seal
      key and a login for the looks'';

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
        The role a judgment runs as, which picks its engine, model and
        effort (`proj settings role`). The plugin offers what that role's
        engine offers, and Conary refuses an answer from any other engine:
        so the role names its model, and moves to no other engine. A look
        runs Claude, so the role's engine is `claude`.
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

    publicKey = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9_-]{43}";
      example = "Mf3hq6hSbtrH0pDB4dPzO2Ff1F6yyhBiR6R-4VYJ8jM";
      description = ''
        The public half of keyFile: the producer's Ed25519 public key, its
        32 bytes in unpadded base64url (Conary's docs/service-attestation.md),
        which the machine's producers file trusts for its judgment check,
        `conary/judgment`. Not a secret,
        so it's kept in the store.
      '';
    };

    sealFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/judge-seal-key";
      description = ''
        The flow's judge seal key (its `settings.judge_key`), as 64
        lowercase hex characters, readable by root: a secret, never in the
        store. The producer and the flow's controller alone get it, each as
        a systemd credential: the controller's through autoland-flow-keys,
        whose copy the flow configurations name (`judge_key_file`).
      '';
    };

    claudeTokenFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/judge-claude-token";
      description = ''
        Claude's login for the looks: a token from `claude setup-token`,
        readable by root, a secret never in the store. Each look alone gets
        it, as its systemd credential. Never the host's
        ~/.claude/.credentials.json: its refresh token rotates, and a look
        refreshing it could log out the host's sessions.
      '';
    };

    hosts = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "api.anthropic.com" ];
      description = ''
        The hosts a look's gate lets it reach, on 443, and nothing else: the
        engine's API.
      '';
    };

    timeoutMinutes = lib.mkOption {
      type = lib.types.ints.positive;
      default = autoland.jobTimeoutMinutes;
      defaultText = lib.literalExpression "config.burrow.autoland.jobTimeoutMinutes";
      description = "How long a look may run before it's stopped and judges nothing.";
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
      # producer and its looks alike.
      default = {
        inherit socket;
        inherit (flow) controller;
        inherit (cfg) producer role hosts;
        credential = "judge-key";
        seal = "judge-seal";
        flow = autoland.flowConfigs;
        settings = config.burrow.settings.effective;
        owner = config.burrow.owner;
        api = "https://api.github.com";
        token = if producerToken == null then null else "${producerToken}";
        gh = if app == null then null else "${app.gh}/bin/gh";
        git = lib.getExe config.programs.git.package;
        github = "https://github.com";
        inherit state;
        unit = look;
        systemctl = "${config.systemd.package}/bin/systemctl";
        timeout = cfg.timeoutMinutes * 60;
        claude = lib.getExe' config.burrow.claude.package "claude";
        login = "claude-token";
      };
      description = "What the plugin, the producer and its looks read.";
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-judge-plugin" ''
        exec ${lib.getExe package} plugin /etc/${etcConfig}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for judgments.";
    };

    choice = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      internal = true;
      default = "${flowFiles}/bin/burrow-flow-files choice ${config.burrow.settings.declared} ${cfg.role}";
      defaultText = lib.literalMD "the command that prints the judging role's choice";
      description = ''
        The command that prints what the flow configuration's `execution`
        asks judgments to run with: the judging role's
        engine, model and effort as the settings source declares them.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
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

    environment.etc.${etcConfig}.source = configFile;

    users.users.${producer} = {
      isSystemUser = true;
      group = producer;
    };
    users.groups.${producer} = { };

    # Beside the App's default askers, not instead of them.
    burrow.github.apps = lib.mkIf (cfg.app != null) {
      ${cfg.app}.users = lib.mkOptionDefault [ producer ];
    };

    # Its mirrors and its looks' folders, for each look's user, in its
    # group, to read the one that's its own.
    systemd.tmpfiles.rules = [ "d ${state} 0750 ${producer} ${producer} -" ];

    systemd.services.${producer} = {
      description = "Conary's judge producer: the flow's judgments, sealed and signed";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (app != null) app.env;
      # With no controller, the assertion above says why, rather than
      # flow.producer's throw.
      serviceConfig = lib.optionalAttrs (flow.controller != null) (flow.producer "judge") // {
        User = producer;
        Group = producer;
        ExecStart = "${lib.getExe package} producer ${configFile}";
        LoadCredential = [ "judge-key:${cfg.keyFile}" "judge-seal:${cfg.sealFile}" ];
        UMask = "0027";
        # A restart drops its requests, whose plugins see their connections
        # close, and stops their looks; what a crash left is finished at
        # its start.
        Restart = "on-failure";
      };
    };

    # One judgment's look: its answer, in its folder, once its unit exits 0
    # (crates/burrow-judge, worker.rs).
    systemd.services."${look}@" = {
      description = "A judge producer's look %i";
      # Its producer, which alone reads what it answers: a look outliving it
      # is no one's.
      bindsTo = [ "${producer}.service" ];
      after = [ "${producer}.service" ];
      unitConfig.CollectMode = "inactive-or-failed";
      # A switch leaves a look be: its producer stops it.
      restartIfChanged = false;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        # A user of its own, named for the look, and the producer's group,
        # for its look's folder, the only part of the producer's state it
        # sees. Other users' processes it doesn't see.
        DynamicUser = true;
        User = "judge-%i";
        ProtectProc = "invisible";
        ProtectHome = true;
        SupplementaryGroups = [ producer ];
        # Nothing of burrow's state but its folder, nothing of the
        # projects', and none of the machine's settings for Claude (its
        # managed settings and CLAUDE.md, which are for authoring).
        TemporaryFileSystem = [ "${stateDir}:ro" "/etc/claude-code:ro" ]
          ++ [ "${config.burrow.projectsDir}:ro" ];
        BindPaths = [ "${state}/looks/%i" ];
        # No network of its own: its one way out is its folder's gate.
        PrivateNetwork = true;
        LoadCredential = [ "claude-token:${cfg.claudeTokenFile}" ];
        ExecStart = "${lib.getExe package} look ${configFile} %i";
        TimeoutStartSec = cfg.timeoutMinutes * 60;
        UMask = "0077";
      };
    };

    # The producer starts and stops its looks' units, and nothing else.
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id != "org.freedesktop.systemd1.manage-units" || subject.user != "${producer}") return polkit.Result.NOT_HANDLED;
        var verb = action.lookup("verb");
        if (verb != "start" && verb != "stop") return polkit.Result.NOT_HANDLED;
        return /^${look}@[0-9a-f]+\.service$/.test(action.lookup("unit")) ? polkit.Result.YES : polkit.Result.NOT_HANDLED;
      });
    '';
  };
}
