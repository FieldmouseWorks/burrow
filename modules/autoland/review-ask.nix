# The owner's door (crates/burrow-review-ask, burrow.autoland.askAsOwner):
# the one thing the pipeline does as the owner, a bare review ask to Codex's
# connector, which takes one from nobody else. Whoever asks (the steward's
# reviewers.py as burrow.user, or Conary's flow in conaryd, its
# `review_ask`, as the flow's user, its controller where it has one) runs
# its command, a client of the door's broker: a service of burrow.user's,
# which runs the door with that user's own gh login, as a person's issue's
# job acts, for those two users only (its peer check). Nothing of the
# asker's environment (its App token, its credentials) reaches the door,
# and the login never leaves burrow.user. Part of burrow.autoland, which
# imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (lib) mkOption types;
  dir = "${stateDir}/review-ask";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-review-ask;
  bin = "${package}/bin/burrow-review-ask";
  # What crates/burrow-review-ask's door reads (its Config).
  doorConfig = pkgs.writeText "burrow-review-ask.json" (builtins.toJSON {
    inherit (cfg.askAsOwner) repos perDay;
    stateDir = dir;
    held = [ cfg.labels.hold cfg.labels.waiting.releaseAge.name ];
  });
  # The door, as the broker runs it with REPO PR HEAD.
  door = pkgs.writeShellScript "burrow-review-ask" ''
    export BURROW_REVIEW_ASK_CONFIG=${doorConfig}
    export PATH=${lib.makeBinPath [ (config.burrow.tools.gh or pkgs.gh) ]}
    exec ${bin} door "$@"
  '';
  # Where the broker listens. Anyone may reach it: its peer check is the
  # gate. Not among the flow's producers, whose directory the steward,
  # burrow.user, may not enter.
  socket = "/run/burrow-review-ask/door.sock";
  # Who may ask: the steward, and the flow (its controller, where it has
  # one; burrow.user otherwise).
  askers = lib.unique [ user config.burrow.ledger.flow.user ];
  # What the broker reads (broker.rs, its Config).
  brokerConfig = pkgs.writeText "burrow-review-ask-broker.json" (builtins.toJSON {
    inherit socket askers;
    door = [ "${door}" ];
  });
  # The door's command as its askers run it, with REPO PR HEAD: the
  # broker's client, whose exit and what it prints are the door's. The
  # broker runs one door a head at a time, ends it within its 150s
  # (inside Conary's flow's 180s deadline for a door, flow/ask.rs), and
  # ends it at once when its asker goes, with all it started (its fence):
  # the flow's kill of the client reaches the door that way, so a door the
  # flow gave up on never posts.
  ask = pkgs.writeShellScript "burrow-review-ask-as-owner" ''
    exec ${bin} ask ${socket} "$@"
  '';
in
{
  options.burrow.autoland.askAsOwner = {
    repos = mkOption {
      type = types.listOf (types.strMatching "[^/]+/[^/]+");
      default = [ ];
      example = [ "sam/homelab" ];
      description = ''
        Repos (OWNER/NAME) where the steward asks Codex's connector for a
        late review as the owner: the connector takes its `@codex review`
        from nobody else, an App included. When a head has no review half
        an hour after its push, or its findings are all answered without a
        push, the steward asks through the owner's door
        (crates/burrow-review-ask), a service of burrow.user's, which posts
        the bare ask with that user's own gh login, once a head, and only on an open, ready PR at
        that head, with no hold or release-age label, that the connector
        hasn't reviewed and has no ask in hand. The steward's own units
        never hold that login, and this is the one thing it does as the
        owner. Elsewhere, a late review goes to the owner's digest.
      '';
    };
    perDay = mkOption {
      type = types.ints.positive;
      default = 20;
      description = "The most review asks the owner's door posts in any 24 hours, across every repo.";
    };
    door = mkOption {
      type = types.nullOr types.attrs;
      readOnly = true;
      internal = true;
      default = if cfg.askAsOwner.repos == [ ] then null else {
        command = [ "${ask}" ];
        inherit socket askers;
        record = "${dir}/asked.json";
        inherit (cfg.askAsOwner) repos;
      };
      description = "The door as its askers are given it (the steward's reviewers.py, the flow's review_ask): its command, the broker's client, its record and its repos (and, for the checks, where the broker listens and who may ask it); null when it asks in none.";
    };
  };

  config = lib.mkIf (cfg.enable && cfg.askAsOwner.repos != [ ]) {
    # Its record of what it asked, which the steward reads.
    systemd.tmpfiles.settings.burrow.${dir}.d = {
      mode = "0700";
      inherit user;
      inherit (config.users.users.${user}) group;
    };

    # The broker: burrow.user, whose gh login the door acts as, so systemd
    # gives it that user's HOME, where the login is kept.
    systemd.services.burrow-review-ask = {
      description = "The owner's door's broker: review asks as the owner, for the steward and the flow";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig = {
        User = user;
        ExecStart = "${bin} broker ${brokerConfig}";
        RuntimeDirectory = "burrow-review-ask";
        RuntimeDirectoryMode = "0755";
        # Its cgroup is the broker's to divide: each door runs in a fence
        # of its own (broker.rs), emptied once the door is done with, so
        # the gh a door runs in a process group of its own goes with it.
        Delegate = true;
        # A restart stops the doors it ran, and their askers see their
        # connections close.
        Restart = "on-failure";
      };
    };
  };
}
