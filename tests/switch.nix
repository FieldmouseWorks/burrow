# HOST-switch's dead man's switch when activating hangs: the activation is
# killed at its deadline, whatever it was stuck on, and the revert lands inside
# the window with everything the half-finished activation stopped running again;
# and the switch says the change wasn't kept instead of asking for a confirm.
# And the switch's own units opt out of systemd-oomd (burrow.ops.oomd).
#
#   nix build .#checks.x86_64-linux.switch -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "switch";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.switch ];
    burrow.switch.enable = true;
    users.users.dev.isNormalUser = true;

    # Running on the confirmed system. A switch to any specialisation below
    # stops it before activating, and only starts it again at the end.
    systemd.services.keeper = {
      wantedBy = [ "multi-user.target" ];
      environment.FLAVOUR = lib.mkDefault "base";
      script = "exec sleep infinity";
    };

    specialisation = {
      # Activating this one takes 75s, past the 60s window; on its own it would
      # finish. /run/current-system only moves at the end of the activation script.
      slow.configuration = {
        environment.etc.switch-test.text = "slow";
        systemd.services.keeper.environment.FLAVOUR = "slow";
        system.activationScripts.slow = "sleep 75";
      };
      # Two ways the activation gets stuck; the third, a unit whose stop never
      # ends (TimeoutStopSec=infinity), holds up the revert too: burrow#312.
      hung-script.configuration = {
        environment.etc.switch-test.text = "hung-script";
        systemd.services.keeper.environment.FLAVOUR = "hung-script";
        system.activationScripts.hung = "sleep infinity";
      };
      hung-start.configuration = {
        environment.etc.switch-test.text = "hung-start";
        systemd.services.keeper.environment.FLAVOUR = "hung-start";
        systemd.services.hung-start = {
          wantedBy = [ "multi-user.target" ];
          serviceConfig.Type = "oneshot";
          script = "sleep infinity";
        };
      };
    };
  };

  testScript = ''
    window = 60

    machine.wait_for_unit("multi-user.target")
    base = machine.succeed("readlink -f /run/current-system").strip()
    # A test VM boots without the system profile nixos-install makes, and the
    # switch reverts to that profile, the last confirmed system: point it at
    # what's running.
    machine.succeed(f"mkdir -p /nix/var/nix/profiles && ln -sfn {base} /nix/var/nix/profiles/system")


    def hangs(name, also=()):
        """Switching to specialisation `name` ends in the revert, inside the window."""
        target = machine.succeed(f"readlink -f {base}/specialisation/{name}").strip()
        # On the machine's clock, from before the switch to the revert's last
        # word: pending gone, or revert-failed written.
        _, out = machine.execute(
            f"start=$(date +%s); BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {target} 2>&1 || true; "
            "until [ ! -e /run/machine-switch/pending ] || [ -e /run/machine-switch/revert-failed ]; do sleep 0.2; done; "
            'echo "took $(($(date +%s) - start))"',
            timeout=2 * window,
        )
        took = int(out.rsplit("took ", 1)[1])
        assert took <= window, f"reverted after {took}s, past the {window}s window: {out}"
        assert "not kept" in out, out
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "still on the unconfirmed system"
        machine.fail("test -e /etc/switch-test")
        machine.fail("test -e /run/machine-switch/pending")
        machine.fail("test -e /run/machine-switch/revert-failed")
        assert "nothing pending" in machine.succeed("machine-switch --status")
        machine.succeed("journalctl -t machine-switch --cursor-file=/tmp/cursor | grep -q 'activation killed'")
        # What the activation stopped is back, as the confirmed system has it.
        for unit in ("keeper", *also):
            machine.wait_until_succeeds(f"systemctl is-active -q {unit}.service", timeout=30)
            env = machine.succeed(f"systemctl show -p Environment --value {unit}.service")
            assert "FLAVOUR=base" in env, f"{unit}: {env}"


    machine.succeed("journalctl -t machine-switch --cursor-file=/tmp/cursor >/dev/null || true")

    with subtest("an activation past its deadline is killed, and the revert lands in the window"):
        hangs("slow")

    with subtest("an activation script that never returns"):
        hangs("hung-script")

    with subtest("a oneshot that never finishes starting"):
        hangs("hung-start")
        machine.fail("systemctl is-active -q hung-start.service")

    with subtest("oomd never picks the switch's own units"):
        slow = machine.succeed(f"readlink -f {base}/specialisation/slow").strip()
        machine.succeed(f"BURROW_CONFIRM_WITHIN=10min machine-switch --toplevel {slow} >/tmp/switch.log 2>&1 &")
        machine.wait_until_succeeds("systemctl is-active -q machine-switch-activate.service")
        for unit in ("machine-switch-activate", "machine-revert"):
            machine.succeed(f"systemctl show -p ManagedOOMPreference {unit}.service | grep -qx ManagedOOMPreference=omit")
        machine.wait_until_succeeds("grep -q '^live: ' /tmp/switch.log", timeout=120)
        machine.succeed("machine-switch --abort")
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "the abort didn't revert"
  '';
}
