# The owner's door (crates/burrow-review-ask, burrow.autoland.askAsOwner):
# the one thing the steward does as the owner, a bare review ask to Codex's
# connector, which takes one from nobody else. The steward (reviewers.py)
# runs it as a unit of burrow.user's own, waited for, so it posts with that
# user's own gh login, as a person's issue's job acts, and the steward's own
# units never hold that login. Part of burrow.autoland, which imports it.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.autoland;
  inherit (config.burrow) user stateDir;
  inherit (lib) mkOption types;
  dir = "${stateDir}/review-ask";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-review-ask;
  # What crates/burrow-review-ask reads (its Config).
  doorConfig = pkgs.writeText "burrow-review-ask.json" (builtins.toJSON {
    inherit (cfg.askAsOwner) repos perDay;
    stateDir = dir;
    held = [ cfg.labels.hold cfg.labels.waiting.releaseAge.name ];
  });
  command = pkgs.writeShellScript "burrow-review-ask" ''
    export BURROW_REVIEW_ASK_CONFIG=${doorConfig}
    export PATH=${lib.makeBinPath [ (config.burrow.tools.gh or pkgs.gh) ]}
    exec ${package}/bin/burrow-review-ask "$@"
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
        push, the steward runs the owner's door (crates/burrow-review-ask)
        in a unit of burrow.user's own, which posts the bare ask with that
        user's own gh login, once a head, and only on an open, ready PR at
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
        command = [ "${command}" ];
        record = "${dir}/asked.json";
        inherit (cfg.askAsOwner) repos;
      };
      description = "The door as the steward's config gives it (reviewers.py): its command, its record and its repos; null when it asks in none.";
    };
  };

  config = lib.mkIf (cfg.enable && cfg.askAsOwner.repos != [ ]) {
    # Its record of what it asked, which the steward reads.
    systemd.tmpfiles.settings.burrow.${dir}.d = {
      mode = "0700";
      inherit user;
      inherit (config.users.users.${user}) group;
    };
  };
}
