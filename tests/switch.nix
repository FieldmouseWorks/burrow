# HOST-switch's dead man's switch when activating hangs: the activation is
# killed at its deadline, whatever it was stuck on, and the revert begins there
# and lands inside the window (on the guest's own time: a loaded host's stolen
# time isn't the switch's) with everything the half-finished activation stopped
# running again; and the switch says the change wasn't kept instead of asking
# for a confirm.
# And the switch's own units opt out of systemd-oomd (burrow.ops.oomd).
#
#   nix build .#checks.x86_64-linux.switch -L
{ self, pkgs }:
assert import ./switch-timeouts.nix { inherit self pkgs; nixpkgs = self.inputs.nixpkgs; };
pkgs.testers.runNixOSTest {
  name = "switch";
  # The module patches switch-to-configuration with an overlay.
  node.pkgsReadOnly = false;

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
      # Activating this one takes 115s, past the 100s window; on its own it would
      # finish. /run/current-system only moves at the end of the activation script.
      slow.configuration = {
        environment.etc.switch-test.text = "slow";
        systemd.services.keeper.environment.FLAVOUR = "slow";
        system.activationScripts.slow = "sleep 115";
      };
      # Unbounded stop timeouts are rejected by checks/switch-timeouts.nix.
      # These activations hang with bounded stops, so the revert can finish.
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
          serviceConfig = { Type = "oneshot"; TimeoutStopSec = "2s"; };
          # A cancelled start must reach SIGKILL rather than block the revert.
          script = "trap '' TERM; exec sleep infinity";
        };
      };
    };
  };

  testScript = ''
    window = 100
    # The activation's share of the window (crates/burrow-switch/src/switch.rs,
    # whose own tests hold it to that), and the time killing it and starting
    # the revert may take on a starved guest (about 6s with the VM held to a
    # tenth of a CPU): the revert begins with a tenth of the window still to go.
    deadline = window * 3 // 4
    grace = window * 15 // 100
    # And it lands inside the window on the guest's own time: what the host
    # took away from the guest's CPU (its stolen time) isn't the switch's. A
    # loaded host has stretched a revert from 3s to 70s (remi#1432); a revert
    # held up inside the guest is still late.
    stolen = "awk '/^cpu /{print $9}' /proc/stat"
    hz = 100

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
        # word: pending gone, or revert-failed written. The timeout only
        # catches a revert that never lands: a guest held to a twentieth of a
        # CPU took over 200s.
        _, out = machine.execute(
            f"echo \"start $(date +%s.%N) $({stolen})\"; BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {target} 2>&1 || true; "
            "until [ ! -e /run/machine-switch/pending ] || [ -e /run/machine-switch/revert-failed ]; do sleep 0.2; done; "
            f'echo "landed $(date +%s.%N) $({stolen})"',
            timeout=3 * window,
        )
        assert "landed " in out, f"the revert hadn't landed {3 * window}s after the switch: {out}"
        start, stolen_by_start = map(float, out.split("start ", 1)[1].split()[:2])
        landed, stolen_by_landing = map(float, out.rsplit("landed ", 1)[1].split()[:2])
        lost = (stolen_by_landing - stolen_by_start) / hz
        # The switch's own word, as it got past whatever held it off.
        log = machine.succeed("journalctl -t machine-switch --cursor-file=/tmp/cursor -o short-unix")
        begun = [float(line.split()[0]) for line in log.splitlines() if "reverting to" in line]
        assert len(begun) == 1, log
        began = begun[0] - start
        took = landed - start - lost
        print(
            f"the revert began {began:.1f}s after the switch and landed {landed - start:.1f}s after it, "
            f"{lost:.1f}s of which the host took from the guest"
        )
        assert began <= deadline + grace, (
            f"the revert began {began:.1f}s after the switch, past the activation's {deadline}s deadline: {out}{log}"
        )
        assert took <= window, f"reverted after {took:.1f}s of the guest's own time, past the {window}s window: {out}{log}"
        assert "activation killed" in log, log
        assert "not kept" in out, out
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "still on the unconfirmed system"
        machine.fail("test -e /etc/switch-test")
        machine.fail("test -e /run/machine-switch/pending")
        machine.fail("test -e /run/machine-switch/revert-failed")
        assert "nothing pending" in machine.succeed("machine-switch --status")
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
        machine.wait_until_succeeds("grep -q '^live: ' /tmp/switch.log", timeout=3 * window)
        machine.succeed("machine-switch --abort")
        assert machine.succeed("readlink -f /run/current-system").strip() == base, "the abort didn't revert"
  '';
}
