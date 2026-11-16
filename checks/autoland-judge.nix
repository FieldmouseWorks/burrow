# The judge plugin and its producer (autoland/judge.nix): off by default,
# with no unit; on, a producer unit as a user of its own, listening in its
# own directory under the producers', which only the flow's controller may
# enter, which alone gets the signing key and the seal key; a runner unit as
# burrow.user (proj's jobs are that user's) with neither; and a plugin
# wrapper the flow configuration names. With no controller it's refused:
# the producer would trust burrow.user, the user authoring jobs run as. And
# on at all it's refused until burrow#1186: the runner and its jobs are
# burrow.user's, so the wiring is checked with that one assertion failing.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  off = machine [ ];
  plugin = {
    producer = "sam/judge";
    keyFile = "/run/secrets/judge-producer-key";
    sealFile = "/run/secrets/judge-seal-key";
  };
  on = machine [{
    burrow.ledger.controllerUser = "conary-flow";
    burrow.autoland.plugins.judge = plugin // { enable = true; };
  }];
  uncontrolled = machine [{ burrow.autoland.plugins.judge = plugin // { enable = true; }; }];
  failing = machine: map (a: a.message) (lib.filter (a: !a.assertion) machine.assertions);
  failed = failing uncontrolled;
  gate = lib.hasInfix "can't be turned on before burrow#1186";
  producer = on.systemd.services.autoland-judge;
  runner = on.systemd.services.burrow-judge-runner;
  read = on.burrow.autoland.plugins.judge.config;
  configFile = builtins.unsafeDiscardStringContext (lib.last (lib.splitString " " producer.serviceConfig.ExecStart));
  plain = builtins.unsafeDiscardStringContext;
  wiring = {
    offByDefault = !(off.systemd.services ? autoland-judge) && !(off.systemd.services ? burrow-judge-runner);
    # On, the isolation gate is the one assertion that fails, so no
    # machine can run the signer before burrow#1186.
    gated = lib.length (failing on) == 1 && gate (lib.head (failing on));
    gatedWithoutController = lib.any gate failed;
    refusedWithoutController = lib.any (lib.hasInfix "plugins.judge: only the flow's controller may ask") failed;
    # The keys are the producer's alone, and it is no authoring user.
    ownUser = producer.serviceConfig.User == "autoland-judge"
      && producer.serviceConfig.User != on.burrow.user
      && !(lib.elem "autoland-judge" (on.users.users.${on.burrow.user}.extraGroups or [ ]));
    keys = producer.serviceConfig.LoadCredential
      == [ "judge-key:/run/secrets/judge-producer-key" "judge-seal:/run/secrets/judge-seal-key" ]
      && !(runner.serviceConfig ? LoadCredential);
    socketDir = producer.serviceConfig.RuntimeDirectory == "conary-producers/judge"
      && producer.serviceConfig.SupplementaryGroups == [ "conary-flow" ];
    # The runner is proj's user's, and only the producer's user may ask it.
    runner = runner.serviceConfig.User == on.burrow.user
      && read.user == "autoland-judge" && read.runner == "/run/burrow-judge-runner/runner.sock"
      && runner.serviceConfig.RuntimeDirectory == "burrow-judge-runner";
    producerCommand = lib.hasSuffix "/bin/burrow-judge producer ${configFile}" (plain producer.serviceConfig.ExecStart);
    runnerCommand = lib.hasSuffix "/bin/burrow-judge runner ${configFile}" (plain runner.serviceConfig.ExecStart);
    plugin = lib.hasInfix "/bin/burrow-judge plugin ${configFile}" (plain on.burrow.autoland.plugins.judge.plugin.text);
    config = read.socket == "/run/conary-producers/judge/producer.sock" && read.controller == "conary-flow"
      && read.role == "judge" && read.producer == "sam/judge" && read.credential == "judge-key"
      && read.seal == "judge-seal" && read.jobs == on.burrow.proj.jobsDir
      && lib.hasSuffix "/bin/proj" (lib.head read.proj) && read.settings == on.burrow.settings.effective
      && read.state == "${on.burrow.stateDir}/autoland-judge" && read.token != null;
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
in pkgs.runCommand "autoland-judge-check" { } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the judge producer is wrong: ${w}'\n") wrong}
  touch $out
''
