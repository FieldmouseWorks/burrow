# The review plugin and its producer (crates/burrow-review-producer;
# Conary's docs/m6-plugins.md, R). Conary's flow asks the plugin for a
# review of a PR's head, and the plugin forwards each request to the
# producer, which runs as a user of its own (autoland-review-producer): it
# reads GitHub as an App (burrow.github.apps), and only the flow's
# controller may ask it (its socket's directory and its peer check).
#
# Its looks run in the reviewers' service (autoland/review.nix), the one
# place Codex's login is lent from: the producer takes a snapshot of the
# head into freshDir (paths.nix), which the reviewers' group reads, and
# queues a fresh look on it, which the service runs in a session of its
# own and answers in its done/. So the producer is in the reviewers' group,
# for their queue; the reviewers' user is never in the producer's.
#
# The key is the sign pass's alone (autoland-review-sign, a user of its
# own, its systemd credential): burrow-events runs it when a review is
# submitted, and it signs each look's publication by the flow once it
# matches what the look recorded. It reads and lets go of the producer's
# looks through the producer's group; the producer, which reads PRs, never
# runs as the user the key is lent to.
#
# Off by default: Conary#314 turns the flow's plugins on, with the
# producer's key and its line in the machine's producers file.
# Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland.plugins.review;
  autoland = config.burrow.autoland;
  inherit (config.burrow) stateDir;
  flow = config.burrow.ledger.flow;
  apps = config.burrow.github.apps;
  app = if cfg.app == null then null else apps.${cfg.app} or null;
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-review-producer;
  inherit (import ./paths.nix config) reviewer reviewDir freshDir;
  producer = "autoland-review-producer";
  sign = "autoland-review-sign";
  state = "${stateDir}/${producer}";
  # The sign pass's own: its records, what it signed, and where it books
  # its next pass, which burrow-events reads.
  signState = "${stateDir}/${sign}";
  # Null with no controller, which the assertion below refuses.
  socket = if flow.producers == null then null else "${flow.producers}/review/producer.sock";
  configFile = pkgs.writeText "burrow-review-producer.json" (builtins.toJSON cfg.config);
  # Its environment: the App's gh first, which git's credential helper
  # (`gh auth git-credential`) is, for the snapshots' fetches.
  appPath = lib.optionals (app != null) [ app.gh ] ++ [ config.programs.git.package ];
in
{
  options.burrow.autoland.plugins.review = {
    enable = lib.mkEnableOption ''
      Conary's review plugin and its producer, whose looks run fresh in the
      reviewers' service and whose key signs the flow's publication of
      each. Off until Conary#314 turns the flow's plugins on; it needs
      burrow.ledger.controllerUser, an App, a key and Codex as a reviewer'';

    producer = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9._/-]+";
      example = "remi/review";
      description = ''
        The producer's name, as the machine's producers file
        (burrow.ledger's evidence.producers) names its public key and the
        checks it's trusted for: `conary/review/ROLE`.
      '';
    };

    role = lib.mkOption {
      type = lib.types.strMatching "[a-z0-9-]+";
      default = "codex";
      description = ''
        The review role the producer answers for, as the flow's settings
        name it (`settings.reviewers`, which must name it alone: a request
        doesn't say which role it's for), and the role in the settings
        source whose model and effort a look runs on when the flow chooses
        none.
      '';
    };

    keyFile = lib.mkOption {
      type = lib.types.str;
      example = "/run/secrets/review-producer-key";
      description = ''
        The producer's Ed25519 secret, its 32 bytes, readable by root: a
        secret, never in the store. The sign pass alone gets it, as its
        systemd credential.
      '';
    };

    app = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = config.burrow.github.machinery;
      defaultText = lib.literalExpression "config.burrow.github.machinery";
      description = ''
        The App, one of burrow.github.apps, the producer reads GitHub and
        fetches as, and the sign pass posts its check runs as. That App
        isn't the trust root: the signed statement is.
      '';
    };

    keepHours = lib.mkOption {
      type = lib.types.ints.positive;
      default = 24;
      description = "How long a look waits for the flow to publish it before it's let go, never signed.";
    };

    config = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      internal = true;
      # What crates/burrow-review-producer reads (its Config): the plugin,
      # the producer and the sign pass alike.
      default = {
        inherit socket;
        inherit (flow) controller;
        inherit (cfg) producer role;
        credential = "review-key";
        flow = autoland.flowConfigs;
        settings = config.burrow.settings.effective;
        upkeep = autoland.upkeepConfig;
        gh = if app == null then null else "${app.gh}/bin/gh";
        inherit state;
        fresh = freshDir;
        queue = "${reviewDir}/queue";
        done = "${reviewDir}/done";
        sign = signState;
        keep = cfg.keepHours * 3600;
      };
      description = "What the plugin, the producer and the sign pass read.";
    };

    plugin = lib.mkOption {
      type = lib.types.path;
      readOnly = true;
      default = pkgs.writeShellScript "burrow-review-plugin" ''
        exec ${package}/bin/burrow-review-plugin plugin ${configFile}
      '';
      defaultText = lib.literalExpression "the plugin's executable";
      description = "The executable the flow configuration's `plugins` names for reviews.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.burrow.ledger.enable && flow.controller != null;
        message = "burrow.autoland.plugins.review: only the flow's controller may ask the producer, so it needs burrow.ledger with its controllerUser.";
      }
      {
        assertion = app != null && lib.elem producer app.users && lib.elem sign app.users;
        message = "burrow.autoland.plugins.review: the producer and its sign pass act on GitHub as an App, so it needs burrow.autoland.plugins.review.app, one of burrow.github.apps, with ${producer} and ${sign} among its users.";
      }
      {
        assertion = autoland.enable && autoland.reviewers.codex.enable && !autoland.dryRun;
        message = "burrow.autoland.plugins.review: its looks run in the reviewers' service, so it needs burrow.autoland with reviewers.codex on and dryRun off.";
      }
    ];

    users.users.${producer} = {
      isSystemUser = true;
      group = producer;
      # The reviewers' queue and answers, which their group writes.
      extraGroups = [ reviewer ];
    };
    users.groups.${producer} = { };
    # The looks, which it reads, marks signed and lets go of.
    users.users.${sign} = {
      isSystemUser = true;
      group = sign;
      extraGroups = [ producer ];
    };
    users.groups.${sign} = { };

    # Beside the App's default askers, not instead of them.
    burrow.github.apps = lib.mkIf (cfg.app != null) {
      ${cfg.app}.users = lib.mkOptionDefault [ producer sign ];
    };

    systemd.tmpfiles.settings.burrow-review-producer = {
      ${state}.d = { mode = "0750"; user = producer; group = producer; };
      "${state}/looks".d = { mode = "2770"; user = producer; group = producer; };
      ${signState}.d = { mode = "0755"; user = sign; group = sign; };
      # A look's snapshot, for the reviewers' group to read, and its
      # session's home, which the reviewers' service makes there.
      ${freshDir}.d = { mode = "2770"; user = producer; group = reviewer; };
    };

    systemd.services.${producer} = {
      description = "Conary's review producer: the flow's reviews, each a fresh look";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (app != null) app.env;
      path = appPath;
      # With no controller, the assertion above says why, rather than
      # flow.producer's throw.
      serviceConfig = lib.optionalAttrs (flow.controller != null) (flow.producer "review") // {
        User = producer;
        Group = producer;
        ExecStart = "${package}/bin/burrow-review-plugin producer ${configFile}";
        # Its snapshots, for the reviewers' group to read.
        UMask = "0027";
        # A restart drops its requests, whose plugins see their
        # connections close, and the looks they queued are let go.
        Restart = "on-failure";
      };
    };

    # The sign pass: when a review is submitted, and again while a look's
    # publication is still to come (its wake file).
    burrow.events.handlers.review-sign = {
      events = [ "pull_request_review" ];
      perRepo = false;
      unit = sign;
      wake = "${signState}/wake.json";
    };
    systemd.services.${sign} = {
      description = "Conary's review producer: sign each look the flow published";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (app != null) app.env;
      path = appPath;
      serviceConfig = {
        Type = "oneshot";
        User = sign;
        Group = sign;
        ExecStart = "${package}/bin/burrow-review-plugin sign ${configFile}";
        LoadCredential = [ "review-key:${cfg.keyFile}" ];
        UMask = "0022";
        TimeoutStartSec = "10min";
      };
    };
  };
}
