# A runner started fresh for one job (modules/dispatch/fresh.nix), for real,
# with a stand-in for the runner: it reads the registration it was handed as
# its credential, leaves a file in its state and finds what a checkout in its
# workspace includes, then ends as its instance says. The dispatcher's user
# hands it over and queues the start, as burrow-dispatch does. Whichever way
# it ends, its registration and its state are gone, and one whose job went
# well doesn't end failed. Confined the way a machine may confine it further
# (ProtectSystem).
#
#   nix build .#checks.x86_64-linux.fresh-runner -L
{ self, pkgs }:
let
  # Instance 1-N: a job that went well; 2-N: one that failed; any other: at
  # its job until it's stopped.
  standIn = pkgs.writeShellScript "runner-stand-in" ''
    set -eu
    [ "$(cat "$CREDENTIALS_DIRECTORY/jit")" = "registration $1" ]
    echo left >"$RUNNER_ROOT/left"
    # What actions/checkout does with the credentials it writes for a
    # private repo's fetch: include them by the path of the workspace under
    # the runner's root, which git matches against the real path.
    w=$RUNNER_ROOT/_work/r
    git init -q "$w"
    printf '[burrow]\n\tseen = yes\n' >"$RUNNER_ROOT/included"
    git -C "$w" config --local "includeIf.gitdir:$w/.git.path" "$RUNNER_ROOT/included"
    [ "$(git -C "$w" config burrow.seen)" = yes ]
    case $1 in
      1-*) exit 0 ;;
      2-*) exit 1 ;;
      *) exec sleep infinity ;;
    esac
  '';
in
pkgs.testers.runNixOSTest {
  name = "fresh-runner";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.dispatch ];
    users.users.dev.isNormalUser = true;
    burrow.dispatch = {
      enable = true;
      fresh = { enable = true; owner = "sam-org"; repos = [ "sam-org/homelab" ]; };
    };
    systemd.services."burrow-fresh-runner@".serviceConfig = {
      ExecStart = lib.mkForce "${standIn} %i";
      ProtectSystem = "strict";
    };
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")

    def hand(instance):
        machine.succeed(
            f"runuser -u dev -- sh -c 'umask 077; echo registration {instance} >/run/burrow-fresh/{instance}'"
        )

    def gone(instance):
        machine.fail(f"test -e /run/burrow-fresh/{instance}")
        machine.fail(f"test -e /var/lib/private/burrow-fresh-runner/{instance}")
        machine.fail(f"test -e /var/lib/burrow-fresh-runner/{instance}")

    # A job that went well: the unit ends well too, its cleanup included,
    # and leaves nothing.
    hand("1-0")
    machine.succeed("systemctl start --wait burrow-fresh-runner@1-0.service")
    gone("1-0")

    # One that failed fails, and leaves nothing either.
    hand("2-0")
    machine.fail("systemctl start --wait burrow-fresh-runner@2-0.service")
    gone("2-0")

    # At its job, started the dispatcher's way: its registration is gone
    # once it's up, and its state once it's stopped.
    hand("3-0")
    machine.succeed("runuser -u dev -- systemctl start --no-block burrow-fresh-runner@3-0.service")
    machine.wait_until_succeeds("systemctl is-active burrow-fresh-runner@3-0.service")
    machine.wait_until_succeeds("test -e /var/lib/private/burrow-fresh-runner/3-0/left")
    machine.fail("test -e /run/burrow-fresh/3-0")
    machine.succeed("systemctl stop burrow-fresh-runner@3-0.service")
    gone("3-0")
  '';
}
