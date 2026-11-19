# A switch re-executes a user manager only when it may need it. A
# re-execution closes the manager's private socket, and every `systemctl
# --user` or `systemd-run --user` in that window was refused ("Failed to
# connect to user scope bus via local transport: Connection refused"), at
# every switch, under a pipeline whose proofs and jobs run as user units
# (PSIV#624: a gate's test failed so, at the moment a switch went live).
# switch-to-configuration, patched by the module (user_reexec.rs), leaves a
# manager running only when it runs the new systemd and its config sources
# are as they were at the re-execution it last saw; a boot starts with none,
# so the first switch re-executes. Each config source counts: the
# generation's user.conf, /run's drop-ins, and the user's own, found from the
# manager process's environment whatever the manager exports to its units.
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
    specialisation.other.configuration = {
      environment.etc.switch-test.text = "other";
      systemd.user.services.mark.environment.FLAVOUR = "other";
    };
    # A user.conf setting only a starting manager applies (systemd's
    # initialize_runtime), with systemd itself unchanged.
    specialisation.slack.configuration = {
      environment.etc.switch-test.text = "slack";
      systemd.user.extraConfig = "TimerSlackNSec=1ms";
    };
    specialisation.slack2.configuration = {
      environment.etc.switch-test.text = "slack2";
      systemd.user.extraConfig = "TimerSlackNSec=1ms";
    };
  };

  testScript = ''
    window = 100
    user = "sudo -u dev XDG_RUNTIME_DIR=/run/user/1000"
    ms = 1000000

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1000.service")
    machine.wait_until_succeeds("test -S /run/user/1000/systemd/private")
    machine.wait_until_succeeds("grep -qx base /run/user/1000/mark")
    base = machine.succeed("readlink -f /run/current-system").strip()
    machine.succeed(f"mkdir -p /nix/var/nix/profiles && ln -sfn {base} /nix/var/nix/profiles/system")
    spec = {
        name: machine.succeed(f"readlink -f {base}/specialisation/{name}").strip()
        for name in ("next", "other", "slack", "slack2")
    }

    def reexecs():
        return int(machine.succeed(
            "journalctl _SYSTEMD_UNIT=user@1000.service --output cat | grep -c '^Reexecuting' || true"
        ))

    def socket():
        return machine.succeed("stat -c %i /run/user/1000/systemd/private").strip()

    def timer_slack():
        pid = machine.succeed("systemctl show -P MainPID user@1000.service").strip()
        return int(machine.succeed(f"cat /proc/{pid}/timerslack_ns"))

    def switch(name):
        """Switches to spec[name] and confirms it; the user manager's
        re-executions meanwhile."""
        before = reexecs()
        rc, out = machine.execute(
            f"BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {spec[name]} 2>&1",
            timeout=2 * window,
        )
        assert rc == 0 and f"live: {spec[name]}" in out, out
        machine.succeed("machine-confirm")
        return reexecs() - before

    with subtest("a boot has no baseline: the first switch re-executes, and records one"):
        assert switch("next") == 1
        machine.succeed("test -s /run/nixos/burrow-user-reexec/1000")

    with subtest("a switch that changes neither systemd nor its config leaves the manager running"):
        inode = socket()
        # What a pipeline does meanwhile: user units started and asked after,
        # through the private socket, for as long as the switch takes.
        machine.succeed(
            f"{user} sh -c 'n=0; while [ ! -e /tmp/switched ]; do"
            " systemctl --user show-environment >/dev/null 2>>/tmp/refused;"
            " systemd-run --user --quiet --collect true 2>>/tmp/refused;"
            " n=$((n+1)); done; echo $n >/tmp/calls' >/dev/null 2>&1 &"
        )
        seen = {"re-executions": switch("other")}
        machine.succeed("touch /tmp/switched")
        machine.wait_until_succeeds("test -s /tmp/calls")
        seen.update({
            "socket bound again": socket() != inode,
            "calls": int(machine.succeed("cat /tmp/calls")),
            "refused": machine.succeed("cat /tmp/refused 2>/dev/null || true"),
        })
        print(seen)
        assert seen["calls"] > 0, seen
        assert seen["re-executions"] == 0 and not seen["socket bound again"], seen
        assert seen["refused"] == "", seen
        # The reload still took the new unit files.
        machine.succeed(f"{user} systemctl --user restart mark.service")
        machine.succeed("grep -qx other /run/user/1000/mark")

    with subtest("the generation's user.conf: a setting added applies, the same again leaves it running"):
        assert timer_slack() != ms
        assert switch("slack") == 1
        assert timer_slack() == ms, timer_slack()
        assert switch("slack2") == 0

    with subtest("a drop-in in /run, outside any generation: added, then removed to reveal user.conf's"):
        machine.succeed("mkdir -p /run/systemd/user.conf.d")
        machine.succeed("printf '[Manager]\\nTimerSlackNSec=2ms\\n' >/run/systemd/user.conf.d/slack.conf")
        assert switch("slack") == 1
        assert timer_slack() == 2 * ms, timer_slack()
        machine.succeed("rm /run/systemd/user.conf.d/slack.conf")
        assert switch("slack2") == 1
        assert timer_slack() == ms, timer_slack()

    with subtest("the generation's user.conf removed: re-executes"):
        # (A removed TimerSlackNSec leaves the slack as it was, re-executed
        # or not: systemd sets it only when one is configured.)
        assert switch("other") == 1
        assert switch("next") == 0

    with subtest("the user's own drop-in counts though the manager exports another config dir"):
        # What the manager hands its units, not what it reads its config by.
        machine.succeed(f"{user} systemctl --user set-environment XDG_CONFIG_HOME=/tmp/empty")
        machine.succeed(
            "sudo -u dev sh -c 'mkdir -p ~/.config/systemd/user.conf.d &&"
            " printf \"[Manager]\\nTimerSlackNSec=3ms\\n\" >~/.config/systemd/user.conf.d/slack.conf'"
        )
        assert switch("other") == 1
        assert timer_slack() == 3 * ms, timer_slack()
        machine.succeed("sudo -u dev rm ~dev/.config/systemd/user.conf.d/slack.conf")
        assert switch("next") == 1

    with subtest("a FIFO or /dev/zero among the drop-ins neither blocks nor fills the switch"):
        machine.succeed(
            "sudo -u dev sh -c 'mkfifo ~/.config/systemd/user.conf.d/fifo.conf &&"
            " ln -s /dev/zero ~/.config/systemd/user.conf.d/zero.conf'"
        )
        assert switch("other") == 1
        assert switch("next") == 0
  '';
}
