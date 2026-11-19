# The flow's controller (burrow.ledger.controllerUser, Conary's
# docs/m6-plugins.md, B3): a user of its own that the flow, its workers and
# so its plugins run as, and the only user that may open a producer's
# socket. Producers listen each in a directory of their own under one only
# the controller's group may enter, so burrow.user, whom every authoring
# session runs as, can't reach one at all; each producer checks its peer
# too (crates/burrow-producer). The ledger stays readable to both, and with
# the option off nothing changes.
#
#   nix build .#checks.x86_64-linux.ledger-controller -L
{ self, pkgs }:
let
  cn = pkgs.writeShellScriptBin "cn" ''
    case $1 in
      init) echo anchored >"$CONARY_LEDGER/anchor" ;;
    esac
  '';
  gh = pkgs.writeShellScriptBin "gh" ''
    case "$*" in
      "auth token") echo gho_stand_in ;;
      *) exit 1 ;;
    esac
  '';
  ledger = {
    enable = true;
    package = cn;
    inherit gh;
    owners = [ "sam" ];
    anchorRepo = "sam/homelab";
    reviewers = [ "steward[bot]" ];
  };
  python = "${pkgs.python3}/bin/python3";
  # A producer's socket as the kit binds it: open to anyone (0666), its
  # peer check the producer's own gate.
  listen = pkgs.writeText "listen.py" ''
    import os, socket
    path = os.path.join(os.environ["RUNTIME_DIRECTORY"], "p.sock")
    s = socket.socket(socket.AF_UNIX)
    s.bind(path)
    os.chmod(path, 0o666)
    s.listen()
    while True:
        c, _ = s.accept()
        c.sendall(b"ok\n")
        c.close()
  '';
  ask = pkgs.writeText "ask.py" ''
    import socket, sys
    s = socket.socket(socket.AF_UNIX)
    s.connect("/run/conary-producers/test/p.sock")
    sys.stdout.write(s.makefile().readline())
  '';
  node = controller: { config, lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = ledger // lib.optionalAttrs controller { controllerUser = "conary-flow"; };
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
    # A producer, as a user of its own, wired as a producer's unit is.
    systemd.services.producer = lib.mkIf controller {
      serviceConfig = config.burrow.ledger.flow.producer "test" // {
        User = "nobody";
        ExecStart = "${python} ${listen}";
      };
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "ledger-controller";

  nodes.on = node true;
  nodes.off = node false;

  testScript = ''
    start_all()
    on.wait_for_unit("multi-user.target")
    off.wait_for_unit("multi-user.target")

    with subtest("the controller is a user of its own, with state only it can read"):
        on.succeed("id conary-flow")
        on.fail("id -nG dev | grep -w conary-flow")
        assert on.succeed("stat -c '%U %G %a' /var/lib/burrow/flow").strip() == "conary-flow conary-flow 700"
        on.fail("runuser -u dev -- ls /var/lib/burrow/flow")
        assert on.succeed("stat -c '%U %G %a' /run/conary-producers").strip() == "root conary-flow 710"

    with subtest("burrow.user can't open a producer's socket, and a flow worker can"):
        on.succeed("systemctl start producer.service")
        on.wait_for_unit("producer.service")
        on.wait_for_file("/run/conary-producers/test/p.sock")
        err = on.fail("runuser -u dev -- ${python} ${ask} 2>&1")
        assert "Permission denied" in err, err
        assert on.succeed("runuser -u conary-flow -- ${python} ${ask}") == "ok\n"
        # As a unit of the flow's runs: as its user, nothing more.
        assert on.succeed("systemd-run --wait --pipe --quiet -p User=conary-flow ${python} ${ask}") == "ok\n"
        assert on.succeed("stat -c '%U %G %a' /run/conary-producers").strip() == "root conary-flow 710"

    with subtest("cn still reads the ledger, as burrow.user and as the controller"):
        on.succeed("systemctl start burrow-ledger@sam-r.service")
        for who in ["dev", "conary-flow"]:
            assert on.succeed(f"runuser -u {who} -- cat /var/lib/burrow/conary/anchor") == "anchored\n", who

    with subtest("with the option off, nothing changes"):
        off.fail("id conary-flow")
        off.fail("test -e /run/conary-producers")
        off.fail("test -e /var/lib/burrow/flow")
  '';
}
