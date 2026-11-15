# The review plugin and its producer (autoland/review-producer.nix): off by
# default, with no unit; on, a producer unit as a user of its own,
# listening in its own directory under the producers', which only the
# flow's controller may enter; a sign pass that alone gets the key, which
# burrow-events runs on a submitted review; the reviewers' service told
# where fresh looks are and let write there; and a plugin wrapper the flow
# configuration names. With no controller it's refused.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  off = machine [ ];
  plugin = {
    producer = "sam/review";
    keyFile = "/run/secrets/review-producer-key";
  };
  on = machine [{
    burrow.ledger.controllerUser = "conary-flow";
    burrow.autoland.plugins.review = plugin // { enable = true; };
  }];
  uncontrolled = machine [{ burrow.autoland.plugins.review = plugin // { enable = true; }; }];
  failed = map (a: a.message) (lib.filter (a: !a.assertion) uncontrolled.assertions);
  producer = on.systemd.services.autoland-review-producer;
  sign = on.systemd.services.autoland-review-sign;
  review = on.systemd.services.autoland-review;
  read = on.burrow.autoland.plugins.review.config;
  configFile = builtins.unsafeDiscardStringContext (lib.last (lib.splitString " " producer.serviceConfig.ExecStart));
  plain = builtins.unsafeDiscardStringContext;
  # The reviewers' service's config, as a switch deploys it.
  reviewConfig = "${on.environment.etc."burrow/review".source}/config.json";
  wiring = {
    offByDefault = !(off.systemd.services ? autoland-review-producer)
      && !(off.systemd.services ? autoland-review-sign)
      && !(off.burrow.events.handlers ? review-sign);
    holds = lib.all (a: a.assertion) on.assertions;
    refusedWithoutController = lib.any (lib.hasInfix "plugins.review: only the flow's controller may ask") failed;
    ownUser = producer.serviceConfig.User == "autoland-review-producer"
      && sign.serviceConfig.User == "autoland-review-producer"
      && lib.elem "autoland-review" on.users.users.autoland-review-producer.extraGroups
      && !(lib.elem "autoland-review-producer" (on.users.users.autoland-review.extraGroups or [ ]));
    socketDir = producer.serviceConfig.RuntimeDirectory == "conary-producers/review"
      && producer.serviceConfig.SupplementaryGroups == [ "conary-flow" ];
    # The key is the sign pass's alone.
    key = sign.serviceConfig.LoadCredential == [ "review-key:/run/secrets/review-producer-key" ]
      && !(producer.serviceConfig ? LoadCredential);
    signedOnReviews = on.burrow.events.handlers.review-sign.events == [ "pull_request_review" ]
      && on.burrow.events.handlers.review-sign.unit == "autoland-review-sign"
      && lib.elem "pull_request_review" on.burrow.events.hookEvents;
    commands = lib.hasSuffix "/bin/burrow-review-plugin producer ${configFile}" (plain producer.serviceConfig.ExecStart)
      && lib.hasSuffix "/bin/burrow-review-plugin sign ${configFile}" (plain sign.serviceConfig.ExecStart)
      && lib.hasInfix "/bin/burrow-review-plugin plugin ${configFile}" (plain on.burrow.autoland.plugins.review.plugin.text);
    config = read.socket == "/run/conary-producers/review/producer.sock" && read.controller == "conary-flow"
      && read.role == "codex" && read.fresh == "/var/cache/autoland/fresh"
      && read.queue == "${on.burrow.stateDir}/autoland/review/queue"
      && read.done == "${on.burrow.stateDir}/autoland/review/done"
      && read.upkeep == on.burrow.autoland.upkeepConfig
      && read.settings == on.burrow.settings.effective;
    # The reviewers' service runs fresh looks there, and may write it.
    reviewers = lib.elem "/var/cache/autoland/fresh" review.serviceConfig.ReadWritePaths;
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
in pkgs.runCommand "autoland-review-producer-check" { } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the review producer is wrong: ${w}'\n") wrong}
  ${pkgs.jq}/bin/jq -e '.fresh == "/var/cache/autoland/fresh"' ${reviewConfig} >/dev/null \
    || fail "the reviewers' service isn't told where fresh looks are"
  touch $out
''
