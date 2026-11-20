# conaryd's flow for landing (modules/autoland/landing.nix), for real: on a
# machine that lists a repo for it, conaryd runs only while burrow-landing
# gives it one. Listed and given nothing, it isn't started at all, so a
# machine without its credentials (the rehearsal of one, whose secrets
# aren't there) has nothing fail when a switch or boot starts the unit.
# Given the repo, conaryd starts on the publication that gives it; taken
# off its last one, its restart stops it and the handback completes; and a
# machine whose marker is missing with the repo still given (one deployed
# from before it) gets conaryd back at burrow-landing's next run. A
# handover's burst of starts doesn't cost burrow-landing its path unit.
# crates/burrow-landing's tests hold the files to the same, without systemd.
#
#   nix build .#checks.x86_64-linux.landing-flow -L
{ self, pkgs }:
let
  # Conary's cn and conaryd, stood in: the build's checks of the files take
  # anything, and conaryd only has to run.
  cn = pkgs.runCommand "cn-stand-in" { } ''
    mkdir -p $out/bin
    printf '#!/bin/sh\nexit 0\n' > $out/bin/cn
    printf '#!/bin/sh\nexec sleep infinity\n' > $out/bin/conaryd
    chmod +x $out/bin/*
  '';
  repo = "acme/widget";
  state = "/var/lib/burrow";
  landing = "${state}/landing";
in
pkgs.testers.runNixOSTest {
  name = "landing-flow";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.autoland self.nixosModules.ledger self.nixosModules.github ];
    burrow.ledger = {
      enable = true;
      package = cn;
      owners = [ "acme" ];
      anchorRepo = repo;
      reviewers = [ "acme-steward[bot]" ];
    };
    burrow.events = {
      host = "example.org";
      secretFile = "/etc/webhook";
    };
    environment.etc.webhook = { text = "stand-in"; mode = "0400"; };
    burrow.autoland = {
      enable = true;
      owners = [ "acme" ];
      authors = [ "acme" ];
      # Not there until the test puts it there: the credential a machine
      # that gives conaryd nothing needn't have.
      app = { id = 1; accountId = 2; slug = "acme-steward"; keyFile = "/run/keys/steward.pem"; };
      shadow = [ repo ];
    };
    burrow.landing.conaryd = [ repo ];
    burrow.settings.allowed = [ repo ];
    users.users.dev.isNormalUser = true;
    environment.systemPackages = [ pkgs.jq ];
  };

  testScript = ''
    import json

    def record():
        return json.loads(machine.succeed("cat ${landing}/owners.json"))["repos"].get("${repo}", {})

    # A run by hand, on top of those its paths start: a burst no start limit
    # refuses, so its path unit keeps listening.
    def landing():
        machine.succeed("systemctl start burrow-landing.service")

    def serial():
        return machine.succeed("cut -d' ' -f1 ${landing}/conaryd-events.serial").strip()

    def started_on_serial():
        machine.wait_until_succeeds("[ \"$(cat ${landing}/conaryd-started)\" = \"$(cut -d' ' -f1 ${landing}/conaryd-events.serial)\" ]", timeout=60)

    machine.wait_for_unit("multi-user.target")
    # The ledger set up, as its first sync leaves it: conaryd's other
    # condition met, as it was in the rehearsal's VM.
    machine.succeed("mkdir -p ${state}/conary && touch ${state}/conary/key.pub")

    with subtest("listed but given nothing, conaryd isn't started, and fails on nothing"):
        landing()
        machine.succeed("jq -e '.repos == []' ${landing}/conaryd-events.json")
        machine.fail("test -e ${landing}/conaryd-given")
        # As a switch or a boot starts it: skipped, not failed on the
        # credentials this machine doesn't have.
        machine.succeed("systemctl start conary-flow.service")
        machine.fail("systemctl -q is-failed conary-flow.service")
        machine.fail("systemctl -q is-active conary-flow.service")
        assert machine.succeed("systemctl show -P ConditionResult conary-flow.service").strip() == "no"

    with subtest("given the repo, conaryd starts on the publication that gives it"):
        machine.succeed("install -D -m 0400 /dev/null /run/keys/steward.pem && echo stand-in > /run/keys/steward.pem")
        machine.succeed("settings landing ${repo} conaryd")
        machine.wait_until_succeeds("jq -e '.repos[\"${repo}\"].to == \"conaryd\"' ${landing}/owners.json", timeout=60)
        # The steward lets go: its pass's release marker, which no pass
        # writes here.
        machine.succeed("install -D -m 0644 /dev/null ${state}/autoland/landing-released.json && echo '{\"${repo}\": 1}' > ${state}/autoland/landing-released.json")
        machine.wait_until_succeeds("jq -e '.repos == [\"${repo}\"]' ${landing}/conaryd-events.json", timeout=90)
        machine.succeed("test -e ${landing}/conaryd-given")
        machine.wait_for_unit("conary-flow.service")
        started_on_serial()
        assert record()["owner"] == "conaryd", record()

    with subtest("a marker gone missing with the repo still given gets conaryd back"):
        # As a deploy from before the marker leaves it: the switch restarts
        # the changed unit, which skips, and burrow-landing's next run puts
        # the marker back and publishes, which starts conaryd again.
        before = serial()
        machine.succeed("rm ${landing}/conaryd-given && systemctl restart conary-flow.service")
        machine.fail("systemctl -q is-active conary-flow.service")
        landing()
        machine.wait_for_unit("conary-flow.service")
        assert int(serial()) == int(before) + 1, (before, serial())
        started_on_serial()

    with subtest("taken off its last repo, conaryd stops, fails on nothing, and the handback completes"):
        machine.succeed("settings landing ${repo} steward")
        machine.wait_until_succeeds("jq -e '.repos[\"${repo}\"] | .owner == \"steward\" and .to == null' ${landing}/owners.json", timeout=120)
        machine.succeed("jq -e '.repos == []' ${landing}/conaryd-events.json")
        machine.fail("test -e ${landing}/conaryd-given")
        started_on_serial()
        machine.fail("systemctl -q is-active conary-flow.service")
        machine.fail("systemctl -q is-failed conary-flow.service")
        machine.fail("systemctl -q is-failed conary-flow-given.service")
        machine.fail("systemctl -q is-failed burrow-landing.service")
        machine.succeed("systemctl -q is-active burrow-landing.path conary-flow-given.path")
  '';
}
