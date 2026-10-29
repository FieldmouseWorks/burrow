# The ledger's pass over one repo, for real, with a stand-in for cn: it sets
# the ledger up only until set-up has finished (`cn init` on a ledger that's
# set up reads the anchor's whole history from GitHub), and GitHub being
# unavailable (cn exits 75) books the repo's next pass instead of failing the
# unit, which would fail a switch running meanwhile. Anything else still
# fails it. As the machine's own App, cn asks gh-token for each repo's token
# and is never given the user's login.
#
#   nix build .#checks.x86_64-linux.ledger -L
{ self, pkgs }:
let
  # GitHub's answers the pass asks gh for: its token, and the repos. Which
  # it was asked, and whether in the App's env, go in /tmp/gh-calls.
  gh = pkgs.writeShellScriptBin "gh" ''
    echo "$1 $2 ''${GH_TOKEN_SOCKET:-none}" >>/tmp/gh-calls
    case "$*" in
      "auth token") echo gho_stand_in ;;
      "repo list sam "*) echo sam/r ;;
      *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
    esac
  '';
  # Notes each call, and whose word it was told the steward's is; `init`
  # keeps the anchor's copy, as set-up's last step does, and `sync` exits
  # with what /tmp/cn-exit says.
  # Whose token it has, and, given a command for each repo's, the one that
  # command prints for the repo it syncs, as cn runs it: split on spaces, no
  # shell.
  cn = pkgs.writeShellScriptBin "cn" ''
    echo "$*" >>/tmp/cn-calls
    echo "$CONARY_REVIEWERS" >/tmp/cn-reviewers
    echo "''${GH_TOKEN:-none} ''${CONARY_TOKEN_COMMAND:-none}" >/tmp/cn-auth
    case $1 in
      init) touch "$CONARY_LEDGER/anchor" ;;
      sync)
        if [ -n "''${CONARY_TOKEN_COMMAND:-}" ]; then $CONARY_TOKEN_COMMAND "$2" >/tmp/cn-token; fi
        exit "$(cat /tmp/cn-exit 2>/dev/null || echo 0)" ;;
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
in
pkgs.testers.runNixOSTest {
  name = "ledger";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = ledger;
    # Only the pass's units: burrow-events would start a full pass, which
    # lists repos on GitHub.
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
  };
  # The same, as the machine's own App, with GitHub's App API a stand-in.
  nodes.app = { lib, ... }: {
    imports = [ self.nixosModules.ledger self.nixosModules.github ];
    burrow.github = {
      apps.workers = {
        id = 7;
        slug = "sam-workers";
        keyFile = "/run/keys/workers.pem";
        api = "http://127.0.0.1:8080";
      };
      machinery = "workers";
    };
    burrow.ledger = ledger;
    burrow.events.enable = lib.mkForce false;
    users.users.dev.isNormalUser = true;
    environment.systemPackages = [ pkgs.openssl ];
    systemd.services.fake-github = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = import ./github-app.nix { inherit pkgs; };
    };
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

    with subtest("only the steward's App's word is its, never the token's own login"):
        assert machine.succeed("cat /tmp/cn-reviewers") == "steward[bot]\n"

    with subtest("as the user, cn has gh's login for every repo"):
        assert machine.succeed("cat /tmp/cn-auth") == "gho_stand_in none\n", machine.succeed("cat /tmp/cn-auth")
        assert machine.succeed("cat /tmp/gh-calls") == "auth token none\n" * 2

    with subtest("as the machine's own App, cn asks for each repo's token, and never has the user's login"):
        app.wait_for_unit("fake-github.service")
        app.succeed("install -d -m 0700 /run/keys && openssl genrsa -traditional -out /run/keys/workers.pem 2048 && chmod 0400 /run/keys/workers.pem")
        app.succeed("systemctl start burrow-ledger@sam-r.service")
        token, command = app.succeed("cat /tmp/cn-auth").strip().split(" ", 1)
        assert token == "none" and command.endswith("/bin/gh-token get"), (token, command)
        assert app.succeed("cat /tmp/cn-token").startswith("ghs_"), app.succeed("cat /tmp/cn-token")
        assert "POST /app/installations/42/access_tokens" in app.succeed("cat /tmp/asked")
        # The full pass lists the owner's repos as the App too.
        app.succeed("systemctl start burrow-ledger.service")
        listed = app.succeed("cat /tmp/gh-calls").splitlines()
        assert listed == ["repo list /run/gh-token/workers.sock"], listed

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
        for bad in ["", "  ", '{"sam/a": 1} {"sam/b": 2}', "[]"]:
            machine.succeed(f"printf %s '{bad}' >/var/lib/burrow/ledger/wake.json")
            machine.fail("systemctl start burrow-ledger@sam-r.service")
            assert machine.succeed("cat /var/lib/burrow/ledger/wake.json") == bad, bad
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
        # Past systemd's start limit by now.
        machine.succeed("systemctl reset-failed burrow-ledger@sam-r.service")

    with subtest("anything else fails it"):
        machine.succeed("echo 1 >/tmp/cn-exit")
        machine.fail("systemctl start burrow-ledger@sam-r.service")
        machine.succeed("systemctl show -p Result burrow-ledger@sam-r.service | grep -x Result=exit-code")
  '';
}
