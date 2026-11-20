# The proof plugin and its producer (crates/burrow-proof; Conary's
# docs/m6-plugins.md, P). Conary's flow asks the plugin to prove or prepare
# a commit's checks, and the plugin forwards each request to the producer,
# which runs as a user of its own (autoland-proof): it alone holds its key
# (a systemd credential) and reads GitHub and posts check runs as an App
# (burrow.github.apps), and only the flow's controller may ask it (its
# socket's directory and its peer check). Each check's recipe is the
# repo's flow settings' (burrow.autoland.flowConfigs), never the request's.
#
# Each run's work is a unit of its own (autoland-proof-work@ID), as a user
# of its own that systemd makes for it and takes back after (DynamicUser),
# since a proof runs the repo's own code. Of the producer's state it sees
# only its run's folder (its spec, its log and its own copy of its repo),
# which its group lets it read and the log write: never another run's
# folder, tree or processes, another repo's mirror, nor the producer's
# key, records or App. The verdict is the work's exit, which the unit
# keeps (0 or 1 leave it active, anything else fails it), and the producer
# alone may start and stop those units (polkit).
#
# Off by default: Conary#314 turns the flow's plugins on, with the
# producer's key and its line in the machine's producers file.
# Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.proof;
  autoland = config.burrow.autoland;
  inherit (config.burrow) stateDir;
  flow = config.burrow.ledger.flow;
  apps = config.burrow.github.apps;
  app = if cfg.app == null then null else apps.${cfg.app} or null;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proof;
  producer = "autoland-proof";
  worker = "autoland-proof-work";
  state = "${stateDir}/autoland-proof";
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/proof/producer.sock";
  configFile = pkgs.writeText "burrow-proof.json" (builtins.toJSON cfg.config);
  # Where the plugin reads its config: not the store path itself, since the
  # config names the flow's configs (`flow`), and those name the plugin.
  etcConfig = "burrow/autoland-proof-plugin.json";
in
{
  options.burrow.autoland.plugins.proof = {
    enable = lib.mkEnableOption ''
      Conary's proof plugin and its producer, which run the flow's proofs as
      programs and sign what they ran. Off until Conary#314 turns the flow's
      plugins on; it needs burrow.ledger.controllerUser, an App and a key'';

    producer = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9._/-]+";
      example = "remi/proof";
      description = ''
        The producer's name, as the machine's producers file
        (burrow.ledger's evidence.producers) names its public key and the
        checks it's trusted for: the proofs' names.
      '';
    };

    keyFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/proof-producer-key";
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
        which the machine's producers file trusts for the repo's proofs'
        checks. Not a secret,
        so it's kept in the store.
      '';
    };

    app = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = config.burrow.github.machinery;
      defaultText = lib.literalExpression "config.burrow.github.machinery";
      description = ''
        The App, one of burrow.github.apps, the producer reads GitHub,
        fetches and posts its check runs as. That App isn't the trust root:
        the signed statement is.
      '';
    };

    timeoutMinutes = lib.mkOption {
      type = lib.types.ints.positive;
      default = autoland.proofTimeoutMinutes;
      defaultText = lib.literalExpression "config.burrow.autoland.proofTimeoutMinutes";
      description = "How long one run, its fetch and clone included, may take before its unit is stopped: no verdict.";
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      internal = true;
      # What crates/burrow-proof reads (its plugin::Config): the plugin, the
      # producer and the worker alike.
      default = {
        inherit socket;
        inherit (flow) controller;
        inherit (cfg) producer;
        credential = "proof-key";
        flow = autoland.flowConfigs;
        gh = if app == null then null else "${app.gh}/bin/gh";
        git = lib.getExe config.programs.git.package;
        github = "https://github.com";
        inherit state;
        unit = worker;
        systemctl = "${config.systemd.package}/bin/systemctl";
        timeout = cfg.timeoutMinutes * 60;
        nix = "${config.nix.package}/bin";
        heavy = if config.burrow.cpu.enable then [ "${config.burrow.tools.heavy}/bin/heavy" "--" ] else null;
        # The unit's own /var/tmp (PrivateTmp, which DynamicUser brings),
        # gone when the unit is: a run's clone and its home.
        trees = "/var/tmp";
      };
      description = "What the plugin, the producer and the worker read.";
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-proof-plugin" ''
        exec ${package}/bin/burrow-proof-plugin plugin /etc/${etcConfig}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for proofs.";
    };

    choice = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      internal = true;
      default = "${package}/bin/burrow-proof-plugin choice";
      defaultText = lib.literalMD "the command that prints the plugin's choice";
      description = ''
        The command that prints what the flow configuration's `execution`
        asks proofs to run with: the plugin's own engine, a
        program's, which offers no model or effort.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.burrow.ledger.enable && flow.controller != null;
        message = "burrow.autoland.plugins.proof: only the flow's controller may ask the producer, so it needs burrow.ledger with its controllerUser.";
      }
      {
        assertion = app != null && lib.elem producer app.users;
        message = "burrow.autoland.plugins.proof: the producer acts on GitHub as an App, so it needs burrow.autoland.plugins.proof.app, one of burrow.github.apps, with ${producer} among its users.";
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

    systemd.tmpfiles.rules = [ "d ${state} 0750 ${producer} ${producer} -" ];

    systemd.services.autoland-proof = {
      description = "Conary's proof producer: the flow's proofs, run and signed";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (app != null) app.env;
      # With no controller, the assertion above says why, rather than
      # flow.producer's throw.
      serviceConfig = lib.optionalAttrs (flow.controller != null) (flow.producer "proof") // {
        User = producer;
        Group = producer;
        ExecStart = "${package}/bin/burrow-proof-plugin producer ${configFile}";
        LoadCredential = [ "proof-key:${cfg.keyFile}" ];
        # Its mirrors and runs' specs, for its worker's group to read.
        UMask = "0027";
        # A restart stops the runs whose requests it lost, and their
        # plugins see their connections close.
        Restart = "on-failure";
      };
    };

    # One run's work: its exit, kept until the producer stops it, is the
    # verdict (crates/burrow-proof, work.rs).
    systemd.services."${worker}@" = {
      description = "A proof producer's run %i";
      # Its producer, which alone reads how it ended: a run outliving it
      # is no one's.
      bindsTo = [ "autoland-proof.service" ];
      after = [ "autoland-proof.service" ];
      # Anything but 0 or 1 is no verdict: the unit fails, and goes.
      unitConfig.CollectMode = "inactive-or-failed";
      # A switch leaves a run be: its producer stops it.
      restartIfChanged = false;
      path = [ pkgs.bash pkgs.coreutils config.programs.git.package config.nix.package ];
      environment = {
        # Its copy of its repo is the producer's, which git (and nix's git)
        # refuses to read unless it's named safe.
        GIT_CONFIG_COUNT = "1";
        GIT_CONFIG_KEY_0 = "safe.directory";
        GIT_CONFIG_VALUE_0 = "${state}/runs/*";
      };
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        # A fail is a verdict too, kept as the unit's success.
        SuccessExitStatus = "1";
        # A user of its own, named for the run (an unnamed one would be
        # named for the template, which every run would share), and the
        # producer's group, for its run's folder, the only part of the
        # producer's state it sees. Other users' processes it doesn't see.
        DynamicUser = true;
        User = "proof-%i";
        ProtectProc = "invisible";
        SupplementaryGroups = [ producer ];
        TemporaryFileSystem = "${state}:ro";
        BindPaths = [ "${state}/runs/%i" ];
        ExecStart = "${package}/bin/burrow-proof work ${configFile} %i";
        TimeoutStartSec = cfg.timeoutMinutes * 60;
        # Its cgroup is the run's to fence its command in.
        Delegate = true;
        UMask = "0027";
      };
    };

    # The producer starts and stops its runs' units, and nothing else.
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id != "org.freedesktop.systemd1.manage-units" || subject.user != "${producer}") return polkit.Result.NOT_HANDLED;
        var verb = action.lookup("verb");
        if (verb != "start" && verb != "stop") return polkit.Result.NOT_HANDLED;
        return /^${worker}@[0-9a-f]+\.service$/.test(action.lookup("unit")) ? polkit.Result.YES : polkit.Result.NOT_HANDLED;
      });
    '';
  };
}
