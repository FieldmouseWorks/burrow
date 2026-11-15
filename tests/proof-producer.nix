# The proof producer's systemd half (modules/autoland/proof.nix), for real:
# it starts only with its key, as a user of its own, listening only for the
# flow's controller. Each run is a unit of its own (autoland-proof-work@ID)
# as a user systemd makes for it, which reads the producer's mirror though
# it's another user's, and whose exit the unit keeps: 0 and 1 are a pass
# and a fail, anything else (a run killed, a tree that isn't the head's) no
# verdict, the unit gone. A build proves its derivation with the machine's
# nix. One run can't reach another's processes or log, nor write the
# producer's mirror; only the producer starts and stops runs, and nothing
# else; and a restarted producer stops the runs it lost. The crate's tests
# (crates/burrow-proof) run the same work with fakes for systemd and
# GitHub; this is systemd's, polkit's and nix's half.
#
#   nix build .#checks.x86_64-linux.proof-producer -L
{ self, pkgs }:
let
  git = "${pkgs.git}/bin/git";
  cn = pkgs.writeShellScriptBin "cn" "exit 0";
  # A flake whose checks are a derivation that builds and one that doesn't.
  flake = pkgs.writeText "flake.nix" ''
    {
      outputs = _: let
        mk = name: script: derivation {
          inherit name;
          system = "x86_64-linux";
          builder = "/bin/sh";
          args = [ "-c" script ];
        };
      in {
        checks.x86_64-linux = {
          ok = mk "proof-ok" "echo ok >$out";
          bad = mk "proof-bad" "exit 1";
        };
      };
    }
  '';
  lock = pkgs.writeText "flake.lock" (builtins.toJSON { nodes.root = { }; root = "root"; version = 7; });
  state = "/var/lib/burrow/autoland-proof";
  mirror = "${state}/git/acme/widget.git";
in
pkgs.testers.runNixOSTest {
  name = "proof-producer";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.autoland self.nixosModules.ledger self.nixosModules.github ];
    burrow.ledger = {
      enable = true;
      package = cn;
      owners = [ "acme" ];
      anchorRepo = "acme/ledger";
      reviewers = [ "steward[bot]" ];
      controllerUser = "conary-flow";
    };
    burrow.events.enable = lib.mkForce false;
    # Its App, which nothing here asks: the crate's tests are GitHub's half.
    burrow.github = {
      apps.workers = {
        id = 7; accountId = 8;
        slug = "acme-workers";
        keyFile = "/run/keys/workers.pem";
        api = "http://127.0.0.1:8080";
      };
      machinery = "workers";
    };
    burrow.autoland.plugins.proof = {
      enable = true;
      producer = "acme/proof";
      keyFile = "/run/keys/proof";
    };
    # Started by the test, once its key is there.
    systemd.services.autoland-proof.wantedBy = lib.mkForce [ ];
    users.users.dev.isNormalUser = true;
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
  };

  testScript = ''
    import json
    import shlex

    def producer(cmd, ok=True):
        run = f"runuser -u autoland-proof -- bash -c {shlex.quote('umask 0027; ' + cmd)}"
        return machine.succeed(run) if ok else machine.fail(run)

    def unit(id):
        return f"autoland-proof-work@{id}.service"

    def shown(id):
        out = machine.succeed(f"systemctl show -p ActiveState,SubState,Result,ExecMainCode,ExecMainStatus {unit(id)}")
        return dict(l.split("=", 1) for l in out.splitlines())

    # The worker's exit, as the producer reads it (producer.rs, `ended`):
    # only one systemd kept as the unit's success.
    def verdict(id):
        s = shown(id)
        if (s["ActiveState"], s["SubState"], s["Result"], s["ExecMainCode"]) == ("active", "exited", "success", "1"):
            return int(s["ExecMainStatus"])
        return None

    def run(id, recipe, mode="proof", tree=None, block=True):
        spec = {"mode": mode, "repo": "acme/widget", "head": head, "tree": tree or tree_of_head,
                "check": "c", "recipe": recipe}
        producer(f"mkdir {state}/runs/{id} && cat >{state}/runs/{id}/spec.json <<'EOF'\n{json.dumps(spec)}\nEOF")
        # A run with no verdict fails its start, which the producer reads
        # from systemd too.
        machine.execute(f"runuser -u autoland-proof -- systemctl start {'--no-block ' if not block else '''}{unit(id)}")
        return verdict(id) if block else None

    def log(id):
        return producer(f"cat /run/autoland-proof-work/{id}/log")

    def stop(id):
        producer(f"systemctl stop {unit(id)} && rm -rf {state}/runs/{id}")
        machine.fail(f"test -e /run/autoland-proof-work/{id}")

    machine.wait_for_unit("multi-user.target")

    with subtest("the producer starts with its key, as a user of its own, for the controller alone"):
        machine.succeed("install -d -m 0700 /run/keys && head -c 32 /dev/urandom >/run/keys/proof && chmod 0400 /run/keys/proof")
        machine.succeed("systemctl start autoland-proof.service")
        machine.wait_for_file("/run/conary-producers/proof/producer.sock")
        assert machine.succeed("stat -c '%U %G %a' ${state} ${state}/git ${state}/runs").split("\n")[:3] == ["autoland-proof autoland-proof 750"] * 3
        machine.fail("runuser -u dev -- test -e /run/conary-producers/proof/producer.sock")
        machine.succeed("runuser -u conary-flow -- test -S /run/conary-producers/proof/producer.sock")

    # The producer's mirror, as its fetch leaves it: the head under
    # refs/proof/, the producer's own and its group's to read.
    out = producer("""
      set -e
      g="${git} -c user.name=t -c user.email=t@t"
      w=$(mktemp -d)
      cd $w
      $g init -q
      cp ${flake} flake.nix
      cp ${lock} flake.lock
      echo f >f
      $g add . && $g commit -qm one
      mkdir -p ${state}/git/acme
      $g init -q --bare ${mirror}
      h=$($g rev-parse HEAD)
      $g push -q ${mirror} HEAD:refs/proof/$h
      $g rev-parse HEAD HEAD^{tree}
    """).split()
    head, tree_of_head = out[0], out[1]

    with subtest("a run's command is its own user's, and its exit, kept by its unit, is the verdict"):
        assert run("a1", {"Run": ["bash", "-c", "test -f f && echo ran-here && id -u >&2"]}) == 0, shown("a1")
        assert "ran-here" in log("a1"), log("a1")
        uid = int(log("a1").split("ran-here")[1].split()[0])
        assert uid not in (0, int(machine.succeed("id -u autoland-proof"))), uid
        # The log is the producer's group's, and nobody else's.
        assert machine.succeed("stat -c '%G %a' /run/autoland-proof-work/a1/log").strip() == "autoland-proof 640"
        machine.fail("runuser -u dev -- cat /run/autoland-proof-work/a1/log")
        stop("a1")
        assert run("a2", {"Run": ["bash", "-c", "exit 3"]}) == 1, shown("a2")
        stop("a2")

    with subtest("a run killed, or a tree that isn't the head's, is no verdict, and its unit goes"):
        assert run("a3", {"Run": ["bash", "-c", "kill -9 $$"]}) is None, shown("a3")
        assert run("a4", {"Run": ["true"]}, tree="0" * 40) is None, shown("a4")
        for id in ["a3", "a4"]:
            machine.wait_until_fails(f"systemctl list-units --all --plain --no-legend {unit(id)} | grep -q .", timeout=30)
            stop(id)

    with subtest("a build proves its derivation, from the producer's mirror, with the machine's nix"):
        assert run("b1", {"Build": "checks.x86_64-linux.ok"}) == 0, log("b1")
        stop("b1")
        assert run("b2", {"Build": "checks.x86_64-linux.bad"}) == 1, log("b2")
        stop("b2")
        # Prepared: built already, so it needs no slot.
        assert run("b3", {"Build": "checks.x86_64-linux.ok"}, mode="prepare") == 0, log("b3")
        stop("b3")

    with subtest("one run can't reach another's processes or log, nor write the producer's mirror"):
        run("c1", {"Run": ["bash", "-c", "exec sleep 300"]}, block=False)
        machine.wait_until_succeeds("pgrep -x sleep", timeout=60)
        pid = machine.succeed("pgrep -x sleep").strip()
        assert run("c2", {"Run": ["bash", "-c", " && ".join([
            f"! kill -0 {pid}",
            f"! test -r /proc/{pid}/environ",
            "! touch /run/autoland-proof-work/c1/log",
            "! cat /run/autoland-proof-work/c1/log",
            "! touch ${mirror}/forged",
            "! cat /run/credentials/autoland-proof.service/proof-key",
        ])]}) == 0, log("c2")
        stop("c2")

    with subtest("only the producer starts and stops runs, and nothing else"):
        machine.fail("runuser -u dev -- systemctl start autoland-proof-work@d1.service")
        machine.fail("runuser -u conary-flow -- systemctl start autoland-proof-work@d1.service")
        producer("systemctl start autoland-proof-work@not-a-run.service", ok=False)
        producer("systemctl restart autoland-proof.service", ok=False)
        producer("systemctl stop nix-daemon.service", ok=False)

    with subtest("a restarted producer stops the runs it lost"):
        machine.succeed(f"systemctl is-active {unit('c1')}")
        machine.succeed("systemctl restart autoland-proof.service")
        machine.wait_for_file("/run/conary-producers/proof/producer.sock")
        machine.wait_until_fails(f"systemctl is-active {unit('c1')}", timeout=60)
        machine.wait_until_fails("pgrep -x sleep", timeout=30)
        machine.succeed("test ! -e /run/autoland-proof-work/c1 && test ! -e ${state}/runs/c1")
  '';
}
