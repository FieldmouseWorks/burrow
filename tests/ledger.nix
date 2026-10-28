# The ledger's pass over one repo, for real, with a stand-in for cn: it sets
# the ledger up only until set-up has finished (`cn init` on a ledger that's
# set up reads the anchor's whole history from GitHub), and GitHub being
# unavailable (cn exits 75) books the repo's next pass instead of failing the
# unit, which would fail a switch running meanwhile. Anything else still
# fails it.
#
#   nix build .#checks.x86_64-linux.ledger -L
{ self, pkgs }:
let
  # GitHub's answers the pass asks gh for: its token and its login.
  gh = pkgs.writeShellScriptBin "gh" ''
    case "$*" in
      "auth token") echo gho_stand_in ;;
      "api user --jq .login") echo sam ;;
      "repo list sam "*) echo sam/r ;;
      *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
    esac
  '';
  # Notes each call; `init` keeps the anchor's copy, as set-up's last step
  # does, and `sync` exits with what /tmp/cn-exit says.
  cn = pkgs.writeShellScriptBin "cn" ''
    echo "$*" >>/tmp/cn-calls
    case $1 in
      init) touch "$CONARY_LEDGER/anchor" ;;
      sync) exit "$(cat /tmp/cn-exit 2>/dev/null || echo 0)" ;;
    esac
  '';
in
pkgs.testers.runNixOSTest {
  name = "ledger";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = {
      enable = true;
      package = cn;
      inherit gh;
      owners = [ "sam" ];
      anchorRepo = "sam/homelab";
    };
    # Only the pass's units: burrow-events would start a full pass, which
    # lists repos on GitHub.
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
  };

  testScript = ''
    import json
    import time

    def calls():
        return machine.succeed("cat /tmp/cn-calls 2>/dev/null || true").splitlines()

    machine.wait_for_unit("multi-user.target")

    with subtest("the first pass sets the ledger up, and the next only syncs"):
        machine.succeed("systemctl start burrow-ledger@sam-r.service")
        assert calls() == ["init", "sync sam/r"], calls()
        machine.succeed("systemctl start burrow-ledger@sam-r.service")
        assert calls() == ["init", "sync sam/r", "sync sam/r"], calls()

    with subtest("GitHub unavailable books the repo's next pass, and the unit hasn't failed"):
        machine.succeed("echo 75 >/tmp/cn-exit")
        before = time.time()
        machine.succeed("systemctl start burrow-ledger@sam-r.service")
        machine.succeed("systemctl show -p Result burrow-ledger@sam-r.service | grep -x Result=success")
        booked = json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))
        assert list(booked) == ["sam/r"], booked
        assert before + 290 < booked["sam/r"] < time.time() + 310, booked

    with subtest("a booking stays, however overdue, when another repo books its own"):
        machine.succeed("""echo '{"sam/old": 1}' >/var/lib/burrow/ledger/wake.json""")
        machine.succeed("systemctl start burrow-ledger@sam-r.service")
        booked = json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))
        assert sorted(booked) == ["sam/old", "sam/r"] and booked["sam/old"] == 1, booked

    with subtest("a booking that can't be written fails the pass, and the bookings stay"):
        machine.succeed("echo 'not json' >/var/lib/burrow/ledger/wake.json")
        machine.fail("systemctl start burrow-ledger@sam-r.service")
        machine.succeed("grep -x 'not json' /var/lib/burrow/ledger/wake.json")
        # The full pass too, where a repo's sync runs as `sync || ...`.
        machine.fail("systemctl start burrow-ledger.service")
        machine.succeed("grep -x 'not json' /var/lib/burrow/ledger/wake.json")
        machine.succeed("rm /var/lib/burrow/ledger/wake.json")
        machine.succeed("systemctl start burrow-ledger.service")
        assert list(json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))) == ["sam/r"]
        machine.succeed("rm /var/lib/burrow/ledger/wake.json")

    with subtest("anything else fails it"):
        machine.succeed("echo 1 >/tmp/cn-exit")
        machine.fail("systemctl start burrow-ledger@sam-r.service")
        machine.succeed("systemctl show -p Result burrow-ledger@sam-r.service | grep -x Result=exit-code")
  '';
}
