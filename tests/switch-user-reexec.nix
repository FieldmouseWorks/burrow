# A switch that keeps systemd leaves each user manager running: it reloads
# it, as before, but no longer re-executes it. A re-execution closes the
# manager's private socket, and every `systemctl --user` or `systemd-run
# --user` in that window was refused ("Failed to connect to user scope bus
# via local transport: Connection refused"), at every switch, under a
# pipeline whose proofs and jobs run as user units (PSIV#624: a gate's test
# failed so, at the moment a switch went live). switch-to-configuration,
# patched by the module (user_reexec.rs), re-executes a user manager only
# when it runs another systemd binary than the new system's, or user.conf
# changed (some of its settings only a starting manager applies), as it does
# PID 1.
#
#   nix build .#checks.x86_64-linux.switch-user-reexec -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "switch-user-reexec";
  # The module patches switch-to-configuration with an overlay.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.switch ];
    burrow.switch.enable = true;
    # The pipeline's user: its manager runs from boot, as a machine's does.
    users.users.dev = { isNormalUser = true; linger = true; };

    # A user unit the switch changes, so its reload is seen to happen.
    systemd.user.services.mark = {
      wantedBy = [ "default.target" ];
      environment.FLAVOUR = lib.mkDefault "base";
      serviceConfig = { Type = "oneshot"; RemainAfterExit = true; };
      script = ''echo "$FLAVOUR" >"$XDG_RUNTIME_DIR/mark"'';
    };

    specialisation.next.configuration = {
      environment.etc.switch-test.text = "next";
      systemd.user.services.mark.environment.FLAVOUR = "next";
    };

    # A user.conf setting only a starting manager applies (systemd's
    # initialize_runtime), with systemd itself unchanged.
    specialisation.slack.configuration = {
      environment.etc.switch-test.text = "slack";
      systemd.user.extraConfig = "TimerSlackNSec=1ms";
    };
  };

  testScript = ''
    window = 100
    user = "sudo -u dev XDG_RUNTIME_DIR=/run/user/1000"

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1000.service")
    machine.wait_until_succeeds("test -S /run/user/1000/systemd/private")
    machine.wait_until_succeeds("grep -qx base /run/user/1000/mark")
    base = machine.succeed("readlink -f /run/current-system").strip()
    machine.succeed(f"mkdir -p /nix/var/nix/profiles && ln -sfn {base} /nix/var/nix/profiles/system")
    nxt = machine.succeed(f"readlink -f {base}/specialisation/next").strip()
    slack = machine.succeed(f"readlink -f {base}/specialisation/slack").strip()

    def reexecs():
        return int(machine.succeed(
            "journalctl _SYSTEMD_UNIT=user@1000.service --output cat | grep -c '^Reexecuting' || true"
        ))

    def socket():
        return machine.succeed("stat -c %i /run/user/1000/systemd/private").strip()

    def timer_slack():
        pid = machine.succeed("systemctl show -P MainPID user@1000.service").strip()
        return int(machine.succeed(f"cat /proc/{pid}/timerslack_ns"))

    with subtest("a switch that keeps systemd reloads the user manager without re-executing it"):
        before, inode = reexecs(), socket()
        # What a pipeline does meanwhile: user units started and asked after,
        # through the private socket, for as long as the switch takes.
        machine.succeed(
            f"{user} sh -c 'n=0; while [ ! -e /tmp/switched ]; do"
            " systemctl --user show-environment >/dev/null 2>>/tmp/refused;"
            " systemd-run --user --quiet --collect true 2>>/tmp/refused;"
            " n=$((n+1)); done; echo $n >/tmp/calls' >/dev/null 2>&1 &"
        )
        rc, out = machine.execute(
            f"BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {nxt} 2>&1",
            timeout=2 * window,
        )
        machine.succeed("touch /tmp/switched")
        machine.wait_until_succeeds("test -s /tmp/calls")
        assert rc == 0 and f"live: {nxt}" in out, out
        seen = {
            "re-executions": reexecs() - before,
            "socket bound again": socket() != inode,
            "calls": int(machine.succeed("cat /tmp/calls")),
            "refused": machine.succeed("cat /tmp/refused 2>/dev/null || true"),
        }
        print(seen)
        assert seen["calls"] > 0, seen
        assert seen["re-executions"] == 0 and not seen["socket bound again"], seen
        assert seen["refused"] == "", seen
        # The reload still took the new unit files.
        machine.succeed(f"{user} systemctl --user restart mark.service")
        machine.succeed("grep -qx next /run/user/1000/mark")
        machine.succeed("machine-confirm")

    with subtest("a switch that changes user.conf still re-executes the user manager"):
        before = reexecs()
        assert timer_slack() != 1000000
        rc, out = machine.execute(
            f"BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {slack} 2>&1",
            timeout=2 * window,
        )
        assert rc == 0 and f"live: {slack}" in out, out
        assert reexecs() - before == 1, out
        # Its setting took: a reload alone leaves it unapplied.
        assert timer_slack() == 1000000, timer_slack()
        machine.succeed("machine-confirm")
  '';
}
