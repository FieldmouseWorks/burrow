# The flow's seal keys (autoland/shadow.nix, burrow#1276), for real: with
# the judge on, each flow config names its seal key's file
# (`judge_key_file`) and holds no key, the key being the judge's sealFile
# as autoland-flow-keys copies its credential, where only the flow's
# controller can read it. Loaded by Conary's own loader as the controller
# (`flow::Config::load`, as conaryd and `cn flow` load it at each pass),
# the config's judge_key is the sealFile's; burrow.user, whom authoring
# sessions run as, reads neither the key nor its file, and nor does a
# producer; and no store path holds it. Both units that run the
# flow (the shadow's and conaryd's) require the keys' unit. With the judge
# off there's no such unit (checks/autoland-flow-plugins.nix).
#
# The configs are the machine's live ones (burrow#1393): with the judging
# role set to no model, as a build's choice would be, nothing is rendered;
# setting one through the settings door renders them, by the path unit on
# the role settings, with the judge's choice on that model, and a change to
# it is rendered again with no deploy, however many come in a row. A
# switch to another base renders that, on the same choice.
#
# The judge's producer and looks aren't started: only the flow's half is
# tested here, and that the producer's config names the same key file
# (tests/judge-look.nix and crates/burrow-judge, which checks the producer
# refuses a flow naming any other, have theirs).
#
#   nix build .#checks.x86_64-linux.autoland-flow-keys -L
{ self, pkgs }:
let
  cn = pkgs.writeShellScriptBin "cn" "exit 0";
  repo = "acme/widget";
  seal = "/run/secrets/judge-seal";
  flowFiles = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-flow-files}/bin/burrow-flow-files";
in
pkgs.testers.runNixOSTest {
  name = "autoland-flow-keys";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.autoland self.nixosModules.ledger self.nixosModules.github ];
    config = {
      burrow.ledger = {
        enable = true;
        package = cn;
        owners = [ "acme" ];
        anchorRepo = repo;
        reviewers = [ "acme-steward[bot]" ];
        controllerUser = "conary-flow";
      };
      burrow.events = {
        host = "example.org";
        secretFile = "/etc/webhook";
      };
      environment.etc.webhook = { text = "stand-in"; mode = "0400"; };
      burrow.github = {
        apps.workers = {
          id = 7; accountId = 8;
          slug = "acme-workers";
          keyFile = "/run/keys/workers.pem";
          api = "http://127.0.0.1:8080";
        };
        machinery = "workers";
      };
      burrow.autoland = {
        enable = true;
        owners = [ "acme" ];
        authors = [ "acme" ];
        app = { id = 1; accountId = 2; slug = "acme-steward"; keyFile = "/run/keys/steward.pem"; };
        shadow = [ repo ];
        plugins.judge = {
          enable = true;
          producer = "acme/judge";
          keyFile = "/run/secrets/judge-producer-key";
          sealFile = seal;
          claudeTokenFile = "/run/secrets/judge-claude-token";
          publicKey = "RBb1DHObOnTDtFA1uTBi9xeBGncjjA_H-miBBJmJKaI";
        };
      };
      burrow.landing.conaryd = [ repo ];
      burrow.settings.allowed = [ repo ];
      # The producer isn't this test's.
      systemd.services.autoland-judge.wantedBy = lib.mkForce [ ];
      users.users.dev.isNormalUser = true;
      environment.systemPackages = [ pkgs.jq ];
      # No GitHub here: a pass would fail, and a switch with it.
      burrow.events.enable = lib.mkForce false;
      # Another base, to switch to: one setting of the steward's changed.
      specialisation.rebased.configuration.burrow.autoland.labels.hold = "parked";
    };
  };

  testScript = { nodes, ... }: let
    config = "${nodes.machine.burrow.autoland.flowConfigs}/${repo}.json";
    keyFile = "/run/autoland-flow-keys/judge-seal";
    role = nodes.machine.burrow.autoland.plugins.judge.role;
  in ''
    machine.wait_for_unit("multi-user.target")

    with subtest("the configs are rendered on the judging role as it's set live"):
        # At boot the judging role names no model, which its jobs would
        # refuse: the render fails, and writes nothing.
        machine.wait_until_succeeds("systemctl show -P Result autoland-flow-configs.service | grep -qv success", timeout=60)
        machine.fail("test -e ${config}")
        machine.succeed("runuser -u dev -- settings offer claude models stand-in-one stand-in-two")
        machine.succeed("runuser -u dev -- settings model ${role} claude stand-in-one high")
        machine.wait_until_succeeds("jq -e '.execution.judge == {engine: \"claude\", model: \"stand-in-one\", effort: \"high\"}' ${config}", timeout=60)
        # Everyone reads it; only the flow's controller writes it.
        machine.succeed("runuser -u dev -- test -r ${config}")
        machine.fail("runuser -u dev -- test -w ${nodes.machine.burrow.autoland.flowConfigs}")
        # A change to the role is read at the next render, with no deploy.
        machine.succeed("runuser -u dev -- settings model ${role} claude stand-in-two")
        machine.wait_until_succeeds("jq -e '.execution.judge == {engine: \"claude\", model: \"stand-in-two\", effort: null}' ${config}", timeout=60)

    with subtest("many changes in a row leave it hearing the next"):
        # More than systemd's default start limit (five in ten seconds).
        for effort in ["low", "medium", "high"] * 4:
            machine.succeed(f"runuser -u dev -- settings model ${role} claude stand-in-one {effort}")
            machine.wait_until_succeeds(f"jq -e '.execution.judge.effort == \"{effort}\"' ${config}", timeout=30)
        machine.succeed("systemctl is-active autoland-flow-configs.path")

    with subtest("the config names the key's file and holds no key"):
        machine.succeed("jq -e '.settings.judge_key_file == \"${keyFile}\" and (.settings | has(\"judge_key\") | not)' ${config}")
        # conaryd's is the same file: its repo's entry names it.
        machine.succeed("jq -e '.flow_config[\"${repo}\"] == \"${config}\"' ${nodes.machine.burrow.landing.eventsTemplate}")
        # And the judge's producer, holding the same sealFile as its own
        # credential, checks the flow names that file before it judges.
        machine.succeed("jq -e '.flow_key == \"${keyFile}\" and .seal == \"judge-seal\"' /etc/burrow/autoland-judge-plugin.json")

    with subtest("both units that run the flow require the keys' unit"):
        for unit in ["conary-flow.service", "autoland-shadow@acme-widget.service"]:
            requires = machine.succeed(f"systemctl show -P Requires {unit}").split()
            after = machine.succeed(f"systemctl show -P After {unit}").split()
            assert "autoland-flow-keys.service" in requires and "autoland-flow-keys.service" in after, (unit, requires, after)

    # The seal key, made here so no build ever saw it: as a secret manager
    # writes one, root's alone, with a trailing newline.
    machine.succeed("install -d -m 0700 /run/secrets && (umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \\n' > ${seal}; echo >> ${seal})")
    key = machine.succeed("tr -d '\\n' < ${seal}")
    assert len(key) == 64, len(key)
    # Its boot start failed without the key, as one would on a machine
    # whose secret isn't there yet.
    machine.succeed("systemctl reset-failed autoland-flow-keys.service || true")
    machine.succeed("systemctl start autoland-flow-keys.service")

    with subtest("the controller loads the config's judge_key, and it is the sealFile's"):
        machine.succeed("runuser -u conary-flow -- ${flowFiles} flow ${config}")
        # The sealFile as the controller may read it here, to compare: it
        # can't open the root's own.
        machine.succeed("install -m 0400 -o conary-flow ${seal} /run/seal-copy")
        machine.succeed("runuser -u conary-flow -- ${flowFiles} judge-key ${config} /run/seal-copy")
        # And refused against any other key.
        machine.succeed("printf '%064d' 0 > /run/seal-other && chown conary-flow /run/seal-other")
        machine.fail("runuser -u conary-flow -- ${flowFiles} judge-key ${config} /run/seal-other")
        machine.succeed("rm /run/seal-copy /run/seal-other")

    with subtest("burrow.user reads neither the key nor its file"):
        machine.fail("runuser -u dev -- cat ${keyFile}")
        machine.fail("runuser -u dev -- cat ${seal}")
        machine.fail("runuser -u dev -- ${flowFiles} flow ${config}")
        # Nor does a producer, which joins the controller's group to reach
        # its socket's directory (burrow.ledger's flow.producer).
        machine.fail("runuser -u autoland-judge -g conary-flow -- cat ${keyFile}")

    with subtest("the key stays readable while the unit is up, and only then"):
        machine.succeed("runuser -u conary-flow -- test -r ${keyFile}")
        machine.succeed("systemctl stop autoland-flow-keys.service")
        machine.fail("test -e ${keyFile}")
        machine.succeed("systemctl start autoland-flow-keys.service")
        machine.succeed("runuser -u conary-flow -- test -r ${keyFile}")

    with subtest("a switch renders the new base on the live choice"):
        machine.succeed("jq -e '.settings.hold_label != \"parked\"' ${config}")
        machine.succeed("/run/current-system/specialisation/rebased/bin/switch-to-configuration test >&2")
        machine.wait_until_succeeds("jq -e '.settings.hold_label == \"parked\" and .execution.judge.model == \"stand-in-one\"' ${config}", timeout=60)

    with subtest("no store path of the machine holds the key"):
        # Its whole closure: the rendered configs and units included.
        # grep exits 1 finding nothing, and 2 on an error.
        held = machine.succeed(f"grep -rlsF -- {key} $(nix-store -qR /run/current-system) || [ $? = 1 ]", timeout=1800)
        assert held.strip() == "", held
        # As a check that grep finds the key where it is.
        machine.succeed(f"grep -qF -- {key} ${seal}")
  '';
}
