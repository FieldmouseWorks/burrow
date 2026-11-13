# HOST-switch behind a long run (burrow#811): a unit the activation restarts
# that is ordered after a oneshot already running, as renovate-sweep is after
# renovate's two hours, would hold the activation past its deadline and revert
# a switch with nothing wrong in it. The switch lets that job go, queued again,
# and goes live; the unit restarts once the run ends. The revert's activation
# does the same.
#
#   nix build .#checks.x86_64-linux.switch-queue -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "switch-queue";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.switch ];
    burrow.switch.enable = true;
    users.users.dev.isNormalUser = true;

    # renovate: started now and then, and left to run when a switch comes.
    systemd.services.long = {
      restartIfChanged = false;
      serviceConfig.Type = "oneshot";
      script = "exec sleep 1000";
    };
    # renovate-sweep: ordered after it, and changed by the switch.
    systemd.services.sweep = {
      wantedBy = [ "multi-user.target" ];
      wants = [ "prep.service" ];
      after = [ "long.service" "prep.service" ];
      environment.FLAVOUR = lib.mkDefault "base";
      serviceConfig = { Type = "oneshot"; RemainAfterExit = true; };
      script = ''echo "$FLAVOUR" >/run/sweep'';
    };
    # A pass the switch starts again with multi-user.target, every switch.
    systemd.services.pass = {
      wantedBy = [ "multi-user.target" ];
      after = [ "long.service" ];
      serviceConfig.Type = "oneshot";
      script = "echo ran >>/run/pass";
    };

    # What the sweep pulls in, done in a moment and gone: queued again with
    # its requirements, the sweep would run it once more each time.
    systemd.services.prep = {
      serviceConfig.Type = "oneshot";
      script = "echo ran >>/run/prep";
    };

    specialisation.behind.configuration = {
      environment.etc.switch-test.text = "behind";
      systemd.services.sweep.environment.FLAVOUR = "behind";
      # A new unit the activation waits on for a while, so the sweep's job
      # queued again is let go again at each look meanwhile.
      systemd.services.slow = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig = { Type = "oneshot"; RemainAfterExit = true; };
        script = "sleep 8";
      };
    };
  };

  testScript = ''
    window = 100

    machine.wait_for_unit("multi-user.target")
    base = machine.succeed("readlink -f /run/current-system").strip()
    machine.succeed(f"mkdir -p /nix/var/nix/profiles && ln -sfn {base} /nix/var/nix/profiles/system")
    behind = machine.succeed(f"readlink -f {base}/specialisation/behind").strip()


    def long_runs():
        machine.succeed("systemctl start --no-block long.service")
        machine.wait_until_succeeds("systemctl list-jobs --no-legend | grep -q '^ *[0-9]* long.service *start *running'")


    def flavour(want):
        machine.wait_until_succeeds(f"grep -qx {want} /run/sweep", timeout=30)


    flavour("base")

    with subtest("a switch goes live while a run the sweep is ordered after goes on"):
        long_runs()
        preps = int(machine.succeed("wc -l </run/prep"))
        # Past the activation's deadline it would say "not kept" and revert.
        rc, out = machine.execute(
            f"BURROW_CONFIRM_WITHIN={window}s machine-switch --toplevel {behind} 2>&1",
            timeout=2 * window,
        )
        assert rc == 0 and f"live: {behind}" in out, out
        assert "sweep.service start waited behind long.service" in out, out
        # And the target the pass is pulled in by, waiting on the pass.
        assert "multi-user.target start waited behind long.service" in out, out
        assert machine.succeed("readlink -f /run/current-system").strip() == behind
        assert machine.succeed("cat /etc/switch-test") == "behind"
        # While it waited on the slow unit, the sweep was let go again at
        # each look, without running what it pulls in again each time.
        ran = int(machine.succeed("wc -l </run/prep")) - preps
        assert ran <= 3, f"prep ran {ran} times during the switch"
        # The run goes on, and the sweep waits its turn, queued again.
        machine.succeed("systemctl list-jobs --no-legend | grep -q 'long.service *start *running'")
        machine.succeed("systemctl list-jobs --no-legend | grep -q 'sweep.service *start *waiting'")
        machine.succeed("grep -qx base /run/sweep")
        assert "pending: " in machine.succeed("machine-switch --status")
        machine.succeed("journalctl -t machine-switch | grep -q 'sweep.service start waited behind long.service'")

    with subtest("once the run ends, the sweep restarts as the new system has it"):
        machine.succeed("systemctl stop long.service")
        flavour("behind")

    with subtest("the revert goes back behind a run too"):
        long_runs()
        rc, out = machine.execute("machine-switch --abort 2>&1", timeout=window)
        assert rc == 0, out
        assert machine.succeed("readlink -f /run/current-system").strip() == base
        machine.fail("test -e /etc/switch-test")
        machine.fail("test -e /run/machine-switch/pending")
        machine.succeed("journalctl -t machine-switch | grep -q 'not confirmed, reverted to'")
        machine.succeed("systemctl stop long.service")
        flavour("base")
  '';
}
