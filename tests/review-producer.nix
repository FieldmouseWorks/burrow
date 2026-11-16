# The review producer's systemd half (modules/autoland/review-producer.nix),
# for real: it starts as a user of its own, listening only for the flow's
# controller, with no key; it meets the reviewers' service only where a
# fresh look needs it to (their queue and answers, and the fresh looks'
# snapshots and homes), and nothing of its own state is theirs or anyone
# else's; and the sign pass alone gets the key, runs as a user of its own
# that the producer never is, and books its next pass where burrow-events
# reads it. The crate's tests
# (crates/burrow-review-producer) run the looks and the signing with fakes
# for GitHub and the service; this is systemd's half.
#
#   nix build .#checks.x86_64-linux.review-producer -L
{ self, pkgs }:
let
  stateDir = "/var/lib/burrow";
  state = "${stateDir}/autoland-review-producer";
  review = "/var/lib/burrow-review-looks";
  steward = "${stateDir}/autoland/review";
  fresh = "${review}/snapshots";
in
pkgs.testers.runNixOSTest {
  name = "review-producer";
  # burrow.autoland's machine pieces patch switch-to-configuration.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [
      self.nixosModules.autoland
      self.nixosModules.ledger
      self.nixosModules.github
      ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs = { }; fileSystems = { }; boot = { }; })
    ];
    burrow.claude.package = pkgs.hello;
    burrow.settings.allowed = [ "sam/r" ];
    burrow.events = { host = "example.org"; secretFile = "/run/secrets/webhook"; };
    burrow.autoland = {
      enable = true;
      owners = [ "sam" ];
      authors = [ "sam" ];
      app = { id = 123456; accountId = 123457; slug = "sams-steward"; keyFile = "/run/secrets/steward"; };
      reviewers.codex.enable = true;
      plugins.review = {
        enable = true;
        producer = "sam/review";
        keyFile = "/run/keys/review";
      };
    };
    burrow.ledger = {
      enable = true;
      package = pkgs.writeShellScriptBin "cn" "exit 0";
      anchorRepo = "sam/box";
      controllerUser = "conary-flow";
    };
    # Its App, which nothing here asks: the crate's tests are GitHub's half.
    burrow.github = {
      apps.workers = {
        id = 7; accountId = 8;
        slug = "sam-workers";
        keyFile = "/run/keys/workers.pem";
        api = "http://127.0.0.1:8080";
      };
      machinery = "workers";
    };
    # Started by the test.
    systemd.services.autoland-review-producer.wantedBy = lib.mkForce [ ];
  };

  testScript = ''
    import shlex

    # Stdin is never the test's console: rm asks there before removing
    # another user's file, as the looks and answers are, and waits forever.
    def as_user(user, cmd, ok=True):
        run = f"runuser -u {user} -- bash -c {shlex.quote(cmd)} </dev/null"
        return machine.succeed(run) if ok else machine.fail(run)

    machine.wait_for_unit("multi-user.target")
    # Nothing here is the pipeline's to run: no steward, no Codex.
    machine.succeed("systemctl mask --runtime autoland.service autoland@.service autoland-review.service autoland-review.path burrow-events.service")
    machine.execute("systemctl stop autoland-review.path 'autoland*' burrow-events.service")

    with subtest("the producer starts as a user of its own, with no key, for the controller alone"):
        machine.succeed("systemctl start autoland-review-producer.service")
        machine.wait_for_file("/run/conary-producers/review/producer.sock")
        assert machine.succeed("stat -c '%U %G %a' ${state} ${state}/looks").split("\n")[:2] == ["autoland-review-producer autoland-review-producer 750", "autoland-review-producer autoland-review-producer 2770"]
        assert machine.succeed("stat -c '%U %G %a' ${fresh}").strip() == "autoland-review autoland-review-askers 2770"
        machine.fail("runuser -u dev -- test -e /run/conary-producers/review/producer.sock")
        machine.fail("runuser -u autoland-review -- test -e /run/conary-producers/review/producer.sock")
        machine.succeed("runuser -u conary-flow -- test -S /run/conary-producers/review/producer.sock")
        machine.succeed("test ! -e /run/credentials/autoland-review-producer.service/review-key")

    with subtest("a fresh look passes between the producer and the reviewers' service, and to no one else"):
        # The producer queues a look on a snapshot of its own, as it does.
        as_user("autoland-review-producer", " && ".join([
            "umask 0027",
            "mkdir ${fresh}/fresh-1",
            "echo code >${fresh}/fresh-1/f",
            "echo '{}' >${review}/queue/codex--fresh-1.json",
        ]))
        # The service reads both, makes the look's home beside it and
        # answers in done/.
        as_user("autoland-review", " && ".join([
            "cat ${fresh}/fresh-1/f",
            "cat ${review}/queue/codex--fresh-1.json",
            "mkdir ${review}/sessions/fresh-1.home",
            "umask 0027 && echo '{}' >${review}/done/codex--fresh-1.json",
            "rm ${review}/queue/codex--fresh-1.json",
        ]))
        # The producer reads the answer and clears what's left.
        as_user("autoland-review-producer", " && ".join([
            "cat ${review}/done/codex--fresh-1.json",
            "rm -r ${fresh}/fresh-1",
        ]))
        # The producer's own state is no one else's, and the looks are no
        # one's but theirs.
        as_user("autoland-review", "ls ${state}", ok=False)
        as_user("autoland-review", "touch ${review}/sessions/fresh-1.home/x && rm -r ${review}/sessions/fresh-1.home")
        as_user("dev", "ls ${fresh}", ok=False)
        as_user("conary-flow", "ls ${fresh}", ok=False)

    with subtest("authoring users cannot forge producer requests or answers"):
        for path in ["${review}/judge-queue/codex--judge-1.json", "${review}/queue/codex--fresh-2.json", "${review}/done/codex--judge-1.json", "${review}/done/codex--fresh-1.json"]:
            as_user("dev", f"echo forged >{path}", ok=False)
        as_user("dev", "mv ${review} ${review}-forged", ok=False)
        as_user("autoland-review-producer", "echo forged >${review}/done/codex--fresh-1.json", ok=False)
        as_user("autoland-review-producer", "rm ${review}/done/codex--fresh-1.json", ok=False)
        # The path unit watches both protected queues as well.
        unit = machine.succeed("systemctl cat autoland-review.path")
        assert "DirectoryNotEmpty=${review}/queue" in unit, unit
        assert "DirectoryNotEmpty=${review}/judge-queue" in unit, unit
        assert "PathExistsGlob=${review}/done/*.json" in unit, unit

    with subtest("a steward queue field cannot request a protected judgment"):
        import json
        entry = dict(reviewer="codex", snap="judge-forged", repo="sam/r", number=1,
                     headRefOid="a" * 40, baseRefName="main", prompt="forged", description="",
                     opened=0, at=0, kind="judge", fresh=dict(model="test", effort="high"))
        as_user("dev", "echo " + shlex.quote(json.dumps(entry)) + " >${steward}/queue/codex--judge-forged.json")
        machine.succeed("systemctl unmask --runtime autoland-review.service && timeout 30s systemctl start autoland-review.service")
        result = json.loads(machine.succeed("cat ${steward}/done/codex--judge-forged.json"))
        assert result["result"] == "error", result
        assert "protected queue" in result["error"], result
        machine.succeed("test ! -e ${review}/done/codex--judge-forged.json")

    ${builtins.readFile ./review-context.py}
    ${builtins.readFile ./review-release.py}

    with subtest("the sign pass alone has the key, as a user the producer isn't, and books its next pass"):
        machine.succeed("install -d -m 0700 /run/keys && head -c 32 /dev/urandom >/run/keys/review && chmod 0400 /run/keys/review")
        # No look waits: it signs nothing, asks GitHub nothing, and books
        # no pass.
        machine.succeed("systemctl start autoland-review-sign.service")
        assert machine.succeed("systemctl show -P Result autoland-review-sign.service").strip() == "success"
        assert machine.succeed("runuser -u dev -- cat ${stateDir}/autoland-review-sign/wake.json").strip() == "{}"
        assert machine.succeed("stat -c '%U' ${stateDir}/autoland-review-sign/records").strip() == "autoland-review-sign"
        # It reads and lets go of the producer's looks; the producer can't
        # touch what it signed.
        as_user("autoland-review-producer", "umask 0027 && echo '{}' >${state}/looks/x.json")
        as_user("autoland-review-sign", "cat ${state}/looks/x.json && rm ${state}/looks/x.json")
        as_user("autoland-review-producer", "touch ${stateDir}/autoland-review-sign/records/forged", ok=False)
        as_user("autoland-review", "ls ${state}/looks", ok=False)
        # Its key was its own while it ran, and is gone with it.
        machine.succeed("test ! -e /run/credentials/autoland-review-sign.service")

    with subtest("the reviewers' service may write the fresh looks"):
        paths = machine.succeed("systemctl show -P ReadWritePaths autoland-review.service")
        assert "${review}" in paths, paths
  '';
}
