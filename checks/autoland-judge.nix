# The judge plugin and its producer (autoland/judge.nix): off by default,
# with no unit; on, a producer unit as a user of its own, listening in its
# own directory under the producers', which only the flow's controller may
# enter, which alone gets the signing key and the seal key; a look unit a
# judgment at a time, as a user systemd makes for it, with its login and
# neither key, seeing only its folder of the producer's state, with no
# network of its own, and that only the producer may start; and a plugin
# wrapper the flow configuration names. With no controller it's refused:
# the producer would trust burrow.user, the user authoring jobs run as.
# Nothing of it is burrow.user's: tests/judge-look.nix tries, as that user,
# to answer in the judge's place, for real.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = modules: (self.nixosConfigurations.example.extendModules { inherit modules; }).config;
  off = machine [ ];
  plugin = {
    producer = "sam/judge";
    keyFile = "/run/secrets/judge-producer-key";
    sealFile = "/run/secrets/judge-seal-key";
    claudeTokenFile = "/run/secrets/judge-claude-token";
  };
  on = machine [{
    burrow.ledger.controllerUser = "conary-flow";
    burrow.autoland.plugins.judge = plugin // { enable = true; };
  }];
  uncontrolled = machine [{ burrow.autoland.plugins.judge = plugin // { enable = true; }; }];
  failing = machine: map (a: a.message) (lib.filter (a: !a.assertion) machine.assertions);
  failed = failing uncontrolled;
  producer = on.systemd.services.autoland-judge;
  look = on.systemd.services."autoland-judge-look@";
  read = on.burrow.autoland.plugins.judge.config;
  configFile = builtins.unsafeDiscardStringContext (lib.last (lib.splitString " " producer.serviceConfig.ExecStart));
  plain = builtins.unsafeDiscardStringContext;
  state = "${on.burrow.stateDir}/autoland-judge";
  wiring = {
    offByDefault = !(off.systemd.services ? autoland-judge) && !(off.systemd.services ? "autoland-judge-look@");
    # On, with a controller, nothing fails: the judging engine is out of
    # authoring sessions' reach.
    builds = failing on == [ ];
    refusedWithoutController = lib.any (lib.hasInfix "plugins.judge: only the flow's controller may ask") failed;
    # The keys are the producer's alone, and it is no authoring user.
    ownUser = producer.serviceConfig.User == "autoland-judge"
      && producer.serviceConfig.User != on.burrow.user
      && !(lib.elem "autoland-judge" (on.users.users.${on.burrow.user}.extraGroups or [ ]));
    keys = producer.serviceConfig.LoadCredential
      == [ "judge-key:/run/secrets/judge-producer-key" "judge-seal:/run/secrets/judge-seal-key" ];
    socketDir = producer.serviceConfig.RuntimeDirectory == "conary-producers/judge"
      && producer.serviceConfig.SupplementaryGroups == [ "conary-flow" ];
    # A look is a user of its own, never burrow.user, with its login and
    # neither key, seeing its folder alone, with no network of its own.
    look = look.serviceConfig.DynamicUser && look.serviceConfig.User == "judge-%i"
      && look.serviceConfig.LoadCredential == [ "claude-token:/run/secrets/judge-claude-token" ]
      && look.serviceConfig.PrivateNetwork && look.serviceConfig.ProtectHome
      && look.serviceConfig.SupplementaryGroups == [ "autoland-judge" ]
      && lib.elem "${on.burrow.stateDir}:ro" look.serviceConfig.TemporaryFileSystem
      && lib.elem "/etc/claude-code:ro" look.serviceConfig.TemporaryFileSystem
      && look.serviceConfig.BindPaths == [ "${state}/looks/%i" ]
      && look.bindsTo == [ "autoland-judge.service" ];
    # The producer alone starts and stops looks.
    polkit = lib.hasInfix ''subject.user != "autoland-judge"'' on.security.polkit.extraConfig
      && lib.hasInfix "/^autoland-judge-look@[0-9a-f]+\\.service$/" on.security.polkit.extraConfig;
    # No runner, and nothing of proj's: a judgment is no burrow.user job.
    noRunner = !(on.systemd.services ? burrow-judge-runner) && !(read ? proj) && !(read ? runner);
    producerCommand = lib.hasSuffix "/bin/burrow-judge producer ${configFile}" (plain producer.serviceConfig.ExecStart);
    lookCommand = lib.hasSuffix "/bin/burrow-judge look ${configFile} %i" (plain look.serviceConfig.ExecStart);
    # The plugin reads the same config where the machine keeps it: the
    # config names the flow's configs, which name the plugin.
    plugin = lib.hasInfix "/bin/burrow-judge plugin /etc/burrow/autoland-judge-plugin.json" (plain on.burrow.autoland.plugins.judge.plugin.text)
      && plain "${on.environment.etc."burrow/autoland-judge-plugin.json".source}" == configFile;
    config = read.socket == "/run/conary-producers/judge/producer.sock" && read.controller == "conary-flow"
      && read.role == "judge" && read.producer == "sam/judge" && read.credential == "judge-key"
      && read.seal == "judge-seal" && read.login == "claude-token" && read.unit == "autoland-judge-look"
      && read.hosts == [ "api.anthropic.com" ] && lib.hasSuffix "/bin/claude" read.claude
      && read.settings == on.burrow.settings.effective
      && read.state == state && read.token != null;
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) wiring);
in pkgs.runCommand "autoland-judge-check" { } ''
  fail() { echo "$*"; exit 1; }
  ${lib.concatMapStrings (w: "fail 'the judge producer is wrong: ${w}'\n") wrong}
  touch $out
''
