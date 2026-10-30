# HOST-switch's dead man's switch when the window runs out mid-activation: the
# revert, refused by switch-to-configuration's lock while the new system is
# still activating, waits for it and then puts the old system back, rather than
# calling itself done because /run/current-system hadn't moved yet; and the
# switch says the change wasn't kept instead of asking for a confirm. And the
# switch's own units opt out of systemd-oomd (burrow.ops.oomd).
#
#   nix build .#checks.x86_64-linux.switch -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "switch";

  nodes.machine = {
    imports = [ self.nixosModules.switch ];
    burrow.switch.enable = true;
    users.users.dev.isNormalUser = true;
    # Activating this one takes 30s, and /run/current-system only moves at the
    # end of the activation script: well past a 10s window.
    specialisation.slow.configuration = {
      environment.etc.switch-test.text = "slow";
      system.activationScripts.slow = "sleep 30";
    };
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    base = machine.succeed("readlink -f /run/current-system").strip()
    # A test VM boots without the system profile nixos-install makes, and the
    # switch reverts to that profile, the last confirmed system: point it at
    # what's running.
    machine.succeed(f"mkdir -p /nix/var/nix/profiles && ln -sfn {base} /nix/var/nix/profiles/system")
    slow = machine.succeed(f"readlink -f {base}/specialisation/slow").strip()

    with subtest("a revert due mid-activation waits for it, then reverts"):
        rc, out = machine.execute(f"BURROW_CONFIRM_WITHIN=10s machine-switch --toplevel {slow} 2>&1")
        # The revert's last word: pending gone, or revert-failed written.
        machine.wait_until_succeeds("[ ! -e /run/machine-switch/pending ] || [ -e /run/machine-switch/revert-failed ]", timeout=120)
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "still on the unconfirmed system"
        machine.fail("test -e /etc/switch-test")
        machine.fail("test -e /run/machine-switch/pending")
        machine.fail("test -e /run/machine-switch/revert-failed")
        assert "nothing pending" in machine.succeed("machine-switch --status")
        assert rc != 0 and "not kept" in out, out

    with subtest("oomd never picks the switch's own units"):
        machine.succeed(f"BURROW_CONFIRM_WITHIN=10min machine-switch --toplevel {slow} >/tmp/switch.log 2>&1 &")
        machine.wait_until_succeeds("systemctl is-active -q machine-switch-activate.service")
        for unit in ("machine-switch-activate", "machine-revert"):
            machine.succeed(f"systemctl show -p ManagedOOMPreference {unit}.service | grep -qx ManagedOOMPreference=omit")
        machine.wait_until_succeeds("grep -q '^live: ' /tmp/switch.log", timeout=120)
        machine.succeed("machine-switch --abort")
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "the abort didn't revert"
  '';
}
