# HOST-switch's dead man's switch when the window runs out mid-activation: the
# revert, refused by switch-to-configuration's lock while the new system is
# still activating, waits for it and then puts the old system back, rather than
# calling itself done because /run/current-system hadn't moved yet; and the
# switch says the change wasn't kept instead of asking for a confirm.
#
#   nix build .#checks.x86_64-linux.switch -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "switch";

  nodes.machine = {
    imports = [ self.nixosModules.switch ];
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
  '';
}
