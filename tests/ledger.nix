# The ledger's passes, for real, with a stand-in for cn: they set the ledger
# up only until set-up has finished (`cn init` on a ledger that's set up
# reads the anchor's whole history from GitHub), and a full pass syncs every
# owner's repos in one `cn sync`, which reads the ledger's log once for them
# all. A repo GitHub was unavailable for books its next pass instead of
# failing the unit, which would fail a switch running meanwhile. A repo that
# failed, or an owner whose repos can't be listed, still fails it, without
# holding up the rest. As the machine's own App, cn asks gh-token for each repo's token
# and is never given the user's login, and so does the gate-reuse lookup's
# token command (tests/ledger-gate-reuse.nix runs the lookup).
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
      "repo list sam "*|"repo list kim "*)
        [ ! -e "/tmp/gh-fail-$3" ] || exit 1
        if [ "$3" = sam ]; then printf 'sam/a\nsam/b\n'; else echo kim/c; fi ;;
      *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
    esac
  '';
  # Notes each call, and whose word it was told the steward's is; `init`
  # keeps the anchor's copy, as set-up's last step does, and `sync` says how
  # each repo ended, as `cn sync --help` has it: done, unless
  # /tmp/cn-outcomes has a line `REPO OUTCOME` for it.
  # Whose token it has, and, given a command for each repo's, the one that
  # command prints for each repo it syncs (/tmp/cn-token-OWNER-NAME), as cn
  # runs it: split on spaces, no shell.
  cn = pkgs.writeShellScriptBin "cn" ''
    echo "$*" >>/tmp/cn-calls
    echo "$CONARY_REVIEWERS" >/tmp/cn-reviewers
    echo "$CONARY_REVIEW_AUTHORS" >/tmp/cn-review-authors
    echo "''${GH_TOKEN:-none} ''${CONARY_TOKEN_COMMAND:-none}" >/tmp/cn-auth
    case $1 in
      init) touch "$CONARY_LEDGER/anchor" ;;
      sync)
        shift
        exit=0
        for repo in "$@"; do
          if [ -n "''${CONARY_TOKEN_COMMAND:-}" ]; then $CONARY_TOKEN_COMMAND "$repo" >"/tmp/cn-token-''${repo/\//-}"; fi
          outcome=done
          if [ -e /tmp/cn-outcomes ]; then
            while read -r r o; do [ "$r" != "$repo" ] || outcome=$o; done </tmp/cn-outcomes
          fi
          case $outcome in
            done) echo "{\"repo\":\"$repo\",\"outcome\":\"done\",\"prs\":[],\"refused\":[]}" ;;
            *)
              echo "{\"repo\":\"$repo\",\"outcome\":\"$outcome\",\"error\":\"$repo went wrong\"}"
              if [ "$outcome" = failed ]; then exit=1; elif [ "$exit" = 0 ]; then exit=75; fi ;;
          esac
        done
        exit "$exit" ;;
    esac
  '';
  ledger = {
    enable = true;
    package = cn;
    inherit gh;
    owners = [ "sam" "kim" ];
    anchorRepo = "sam/homelab";
    reviewers = [ "steward[bot]" ];
  };
in
pkgs.testers.runNixOSTest {
  name = "ledger";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.ledger ];
    burrow.ledger = ledger;
    # The test starts the full pass more often than systemd's start limit
    # lets a unit start.
    systemd.services.burrow-ledger.startLimitIntervalSec = 0;
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
        id = 7; accountId = 8;
        slug = "sam-workers";
        keyFile = "/run/keys/workers.pem";
        api = "http://127.0.0.1:8080";
      };
      machinery = "workers";
    };
    burrow.ledger = ledger // {
      # Only sam has the App.
      owners = [ "sam" ];
      gateReuse = { enable = true; repos = [ "sam/a" ]; };
    };
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
        machine.succeed("systemctl start burrow-ledger@sam-a.service")
        assert calls() == ["init", "sync sam/a"], calls()
        machine.succeed("systemctl start burrow-ledger@sam-a.service")
        assert calls() == ["init", "sync sam/a", "sync sam/a"], calls()

    with subtest("only the steward's App's word is its, never the token's own login"):
        assert machine.succeed("cat /tmp/cn-reviewers") == "steward[bot]\n"

    with subtest("the connector's posts are the reviewer's word, recorded unattested"):
        assert machine.succeed("cat /tmp/cn-review-authors") == "chatgpt-codex-connector[bot]\n"

    with subtest("as the user, cn has gh's login for every repo"):
        assert machine.succeed("cat /tmp/cn-auth") == "gho_stand_in none\n", machine.succeed("cat /tmp/cn-auth")
        assert machine.succeed("cat /tmp/gh-calls") == "auth token none\n" * 2

    with subtest("a full pass syncs every owner's repos in one cn sync"):
        machine.succeed("rm /tmp/cn-calls")
        machine.succeed("systemctl start burrow-ledger.service")
        assert calls() == ["sync sam/a sam/b kim/c"], calls()
        machine.succeed("journalctl -u burrow-ledger.service | grep -F 'syncing 3 repos in one cn sync'")

    with subtest("as the machine's own App, cn asks for each repo's token, and never has the user's login"):
        app.wait_for_unit("fake-github.service")
        app.succeed("install -d -m 0700 /run/keys && openssl genrsa -traditional -out /run/keys/workers.pem 2048 && chmod 0400 /run/keys/workers.pem")
        app.succeed("systemctl start burrow-ledger@sam-a.service")
        token, command = app.succeed("cat /tmp/cn-auth").strip().split(" ", 1)
        assert token == "none" and command.endswith("/bin/gh-token get"), (token, command)
        assert app.succeed("cat /tmp/cn-token-sam-a").startswith("ghs_"), app.succeed("cat /tmp/cn-token-sam-a")
        assert "POST /app/installations/42/access_tokens" in app.succeed("cat /tmp/asked")
        # The full pass lists the owner's repos as the App too.
        app.succeed("systemctl start burrow-ledger.service")
        listed = app.succeed("cat /tmp/gh-calls").splitlines()
        assert listed == ["repo list /run/gh-token/workers.sock"], listed

    with subtest("the gate-reuse lookup's token is the App's too, for the repo it's asked about"):
        env = app.succeed("systemctl show -P Environment conary-gate-reuse.service").split()
        path = next(e for e in env if e.startswith("CONARY_GATE_REUSE_CONFIG=")).split("=", 1)[1]
        command = json.loads(app.succeed(f"cat {path}"))["token_command"]
        # With nothing of the App's env: the command brings what it needs.
        token = app.succeed(f"su dev -s /bin/sh -c 'env -i {command} sam/a'")
        assert token.startswith("ghs_") and token == app.succeed("cat /tmp/cn-token-sam-a"), token

    with subtest("GitHub unavailable books the repo's next pass, and the unit hasn't failed"):
        machine.succeed("echo 'sam/a unavailable' >/tmp/cn-outcomes")
        before = time.time()
        machine.succeed("systemctl start burrow-ledger@sam-a.service")
        machine.succeed("systemctl show -p Result burrow-ledger@sam-a.service | grep -x Result=success")
        booked = json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))
        assert list(booked) == ["sam/a"], booked
        assert before + 290 < booked["sam/a"] < time.time() + 310, booked

    with subtest("a booking stays, however overdue, when another repo books its own"):
        machine.succeed("""echo '{"sam/old": 1}' >/var/lib/burrow/ledger/wake.json""")
        machine.succeed("systemctl start burrow-ledger@sam-a.service")
        booked = json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))
        assert sorted(booked) == ["sam/a", "sam/old"] and booked["sam/old"] == 1, booked

    with subtest("in a full pass, the unavailable repo is booked, earlier bookings stay, and the pass hasn't failed"):
        machine.succeed("""echo '{"sam/old": 1}' >/var/lib/burrow/ledger/wake.json""")
        machine.succeed("echo 'sam/b unavailable' >/tmp/cn-outcomes")
        machine.succeed("rm /tmp/cn-calls")
        machine.succeed("systemctl start burrow-ledger.service")
        assert calls() == ["sync sam/a sam/b kim/c"], calls()
        booked = json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))
        assert sorted(booked) == ["sam/b", "sam/old"] and booked["sam/old"] == 1, booked
        machine.succeed("echo 'sam/a unavailable' >/tmp/cn-outcomes")

    with subtest("a booking that can't be written fails the pass, and the bookings stay"):
        for bad in ["", "  ", '{"sam/a": 1} {"sam/b": 2}', "[]"]:
            machine.succeed(f"printf %s '{bad}' >/var/lib/burrow/ledger/wake.json")
            machine.fail("systemctl start burrow-ledger@sam-a.service")
            assert machine.succeed("cat /var/lib/burrow/ledger/wake.json") == bad, bad
        machine.succeed("echo 'not json' >/var/lib/burrow/ledger/wake.json")
        machine.fail("systemctl start burrow-ledger@sam-a.service")
        machine.succeed("grep -x 'not json' /var/lib/burrow/ledger/wake.json")
        # The full pass too, where the other repos are done.
        machine.fail("systemctl start burrow-ledger.service")
        machine.succeed("grep -x 'not json' /var/lib/burrow/ledger/wake.json")
        machine.succeed("rm /var/lib/burrow/ledger/wake.json")
        machine.succeed("systemctl start burrow-ledger.service")
        assert list(json.loads(machine.succeed("cat /var/lib/burrow/ledger/wake.json"))) == ["sam/a"]
        machine.succeed("rm /var/lib/burrow/ledger/wake.json")
        # Past systemd's start limit by now.
        machine.succeed("systemctl reset-failed burrow-ledger@sam-a.service")

    with subtest("anything else fails it"):
        machine.succeed("echo 'sam/a failed' >/tmp/cn-outcomes")
        machine.fail("systemctl start burrow-ledger@sam-a.service")
        machine.succeed("systemctl show -p Result burrow-ledger@sam-a.service | grep -x Result=exit-code")

    with subtest("in a full pass, a failed repo is named and fails it, and the rest are synced"):
        machine.succeed("echo 'sam/b failed' >/tmp/cn-outcomes")
        machine.succeed("rm /tmp/cn-calls")
        machine.fail("systemctl start burrow-ledger.service")
        assert calls() == ["sync sam/a sam/b kim/c"], calls()
        machine.succeed("journalctl -u burrow-ledger.service | grep -F \"couldn't sync sam/b: sam/b went wrong\"")
        machine.fail("test -e /var/lib/burrow/ledger/wake.json")

    with subtest("an owner whose repos can't be listed fails the pass, and the others' are synced"):
        machine.succeed("rm /tmp/cn-outcomes /tmp/cn-calls && touch /tmp/gh-fail-sam")
        machine.fail("systemctl start burrow-ledger.service")
        assert calls() == ["sync kim/c"], calls()
        machine.succeed("journalctl -u burrow-ledger.service | grep -F \"couldn't list sam's repos\"")
  '';
}
