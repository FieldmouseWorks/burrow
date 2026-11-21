# The owner's door's broker (autoland/review-ask.nix, crates/burrow-review-ask
# broker.rs), for real, with the flow's controller of its own
# (burrow.ledger.controllerUser), as conaryd runs it in a repo the door asks
# in (burrow#1379):
#
# - the controller asks through the door's command, the broker's client, and
#   gets what the door said and its exit;
# - the door runs as burrow.user, with that user's home, where its gh login
#   is, and none of the asker's environment (a token in it goes nowhere);
# - burrow.user, as the steward, asks the same way;
# - another user is refused by the broker's peer check, and no door runs;
# - the controller reads nothing of burrow.user's gh login, and nothing of
#   it comes back in an answer.
#
# gh is a stand-in that says who ran it and fails, so nothing is posted.
# The crate's tests have the rest: a door stopped when its asker goes, one
# past its limit, one door a head at a time.
#
#   nix build .#checks.x86_64-linux.autoland-review-ask -L
{ self, pkgs }:
let
  cn = pkgs.writeShellScriptBin "cn" "exit 0";
  repo = "acme/widget";
  head = "0123456789abcdef0123456789abcdef01234567";
  # The owner's login, as gh keeps it: a marker no answer may carry.
  login = "gho_the-owners-own-login";
  gh = pkgs.writeShellScriptBin "gh" ''
    {
      echo "user $(${pkgs.coreutils}/bin/id -un)"
      echo "home $HOME"
      test -r "$HOME/.config/gh/hosts.yml" && echo "reads its login"
      echo "token ''${GH_TOKEN:-none}"
    } > /tmp/gh-ran
    echo "gh: stood in" >&2
    exit 1
  '';
in
pkgs.testers.runNixOSTest {
  name = "autoland-review-ask";

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
      burrow.tools.gh = lib.mkForce gh;
      burrow.autoland = {
        enable = true;
        owners = [ "acme" ];
        authors = [ "acme" ];
        app = { id = 1; accountId = 2; slug = "acme-steward"; keyFile = "/run/keys/steward.pem"; };
        shadow = [ repo ];
        askAsOwner.repos = [ repo ];
      };
      burrow.landing.conaryd = [ repo ];
      burrow.settings.allowed = [ repo ];
      users.users.dev.isNormalUser = true;
      users.users.other.isNormalUser = true;
    };
  };

  testScript = { nodes, ... }: let
    door = nodes.machine.burrow.autoland.askAsOwner.door;
    ask = builtins.head door.command;
  in ''
    import json

    def asks(user, repo, env=""):
        """The door's command as `user` runs it: its exit and what it printed."""
        return machine.execute(f"runuser -u {user} -- env {env} ${ask} {repo} 5 ${head}")

    machine.wait_for_unit("multi-user.target")
    machine.succeed(
        "install -d -o dev -m 0700 /home/dev/.config /home/dev/.config/gh",
        "printf 'github.com:\\n    oauth_token: ${login}\\n' > /home/dev/.config/gh/hosts.yml",
        "chown dev /home/dev/.config/gh/hosts.yml && chmod 0600 /home/dev/.config/gh/hosts.yml",
    )
    machine.wait_for_unit("burrow-review-ask.service")
    machine.wait_for_file("${door.socket}")

    with subtest("the broker runs as burrow.user, and lets the steward and the controller ask"):
        assert machine.succeed("systemctl show -p User --value burrow-review-ask").strip() == "dev"
        askers = sorted(json.loads('${builtins.toJSON door.askers}'))
        assert askers == ["conary-flow", "dev"], askers

    with subtest("the controller gets the door's answer and its exit"):
        code, out = asks("conary-flow", "acme/elsewhere")
        assert code == 2, (code, out)
        said = json.loads(out.strip().splitlines()[-1])
        assert said == {"outcome": "declined", "why": "acme/elsewhere isn't a repo the owner asks in"}, said
        machine.fail("test -e /tmp/gh-ran")

    with subtest("its door runs as burrow.user, with its login and none of the controller's environment"):
        code, out = asks("conary-flow", "${repo}", env="GH_TOKEN=the-controllers-token")
        assert code == 1, (code, out)
        said = json.loads(out.strip().splitlines()[-1])
        assert said["outcome"] == "failed", said
        ran = machine.succeed("cat /tmp/gh-ran")
        assert ran == "user dev\nhome /home/dev\nreads its login\ntoken none\n", ran
        assert "${login}" not in out, out

    with subtest("the steward, burrow.user, asks the same way"):
        machine.succeed("rm /tmp/gh-ran")
        code, out = asks("dev", "${repo}")
        assert code == 1 and "may not ask" not in out, (code, out)
        machine.succeed("test -e /tmp/gh-ran")

    with subtest("anyone else is refused, and no door runs"):
        machine.succeed("rm /tmp/gh-ran")
        uid = machine.succeed("id -u other").strip()
        code, out = asks("other", "${repo}")
        assert code == 1, (code, out)
        said = json.loads(out.strip().splitlines()[-1])
        assert said == {"outcome": "failed", "why": f"the owner's door's broker: uid {uid} may not ask"}, said
        machine.fail("test -e /tmp/gh-ran")

    with subtest("the controller reads nothing of the owner's login"):
        machine.fail("runuser -u conary-flow -- cat /home/dev/.config/gh/hosts.yml")
        machine.fail("runuser -u conary-flow -- ls /home/dev/.config/gh")
        environ = machine.succeed("cat /proc/$(systemctl show -p MainPID --value burrow-review-ask)/environ | tr '\\0' '\\n'")
        assert "${login}" not in environ and "GH_TOKEN" not in environ, environ
  '';
}
