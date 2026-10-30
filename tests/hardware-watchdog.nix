# burrow.ops.watchdogIdentity: with two watchdogs and the wrong one first as
# /dev/watchdog0 (softdog loads in the initrd, the i6300ESB after it), systemd
# pets the named one, and HOST-health's watchdog check passes. The control,
# without the option, pets /dev/watchdog0, softdog (remi on 2026-10-29, whose
# systemd petted the overclocking watchdog rather than iTCO), and the same
# check, run there, fails. Then the control switches, as deploys and reverts
# would: onto the named watchdog, from it to another, back, and off the option
# again, with systemd on the right one and only that one armed each time.
#
#   nix build .#checks.x86_64-linux.hardware-watchdog -L
{ self, pkgs }:
let
  machine = identity: {
    imports = [ self.nixosModules.ops ];
    burrow.ops = { enable = true; watchdogIdentity = identity; };
    boot.initrd.systemd.enable = true;
    boot.initrd.kernelModules = [ "softdog" ];
    virtualisation.qemu.options = [ "-device i6300esb" ];
    # No disk here has SMART for smartd to watch: it fails, and so would the
    # switch below.
    services.smartd.enable = pkgs.lib.mkForce false;
  };
in
pkgs.testers.runNixOSTest {
  name = "hardware-watchdog";

  nodes.guarded = machine "i6300ESB timer";
  nodes.control = {
    imports = [ (machine null) ];
    specialisation.pinned.configuration.burrow.ops.watchdogIdentity = pkgs.lib.mkForce "i6300ESB timer";
    specialisation.soft.configuration.burrow.ops.watchdogIdentity = pkgs.lib.mkForce "Software Watchdog";
  };

  testScript = { nodes, ... }: let
    # HOST-health as heartbeat runs it, over the checks the running system installs.
    health = pkgs.lib.getExe nodes.control.burrow.tools.health;
    # The guarded machine's installed watchdog check, to run on the control too.
    check = nodes.guarded.environment.etc."burrow/health.d/watchdog".source;
  in ''
    def armed(m):
        """The identities of the watchdogs that are armed: systemd is the only
        thing here that opens one."""
        return m.succeed(
            "for w in /sys/class/watchdog/watchdog*; do"
            " if grep -qx active $w/state; then cat $w/identity; fi; done"
        ).splitlines()

    def checks(m):
        """HOST-health's verdicts on m, by check name ("ok" or "fail")."""
        out = m.succeed("${health}").splitlines()
        return dict(l.removeprefix("check:").split(" ", 1) for l in out if l.startswith("check:"))

    start_all()
    for m in (guarded, control):
        m.wait_for_unit("multi-user.target")
        # Both watchdogs are there, softdog first. The i6300ESB's driver loads
        # when udev gets to its device, which can be after multi-user.target.
        m.succeed("grep -qx 'Software Watchdog' /sys/class/watchdog/watchdog0/identity")
        m.wait_until_succeeds("grep -qx 'i6300ESB timer' /sys/class/watchdog/watchdog1/identity")
    base = control.succeed("readlink -f /run/current-system").strip()

    with subtest("systemd pets the named watchdog, not watchdog0"):
        guarded.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog1/state")
        assert armed(guarded) == ["i6300ESB timer"], armed(guarded)
        assert checks(guarded).get("watchdog") == "ok", checks(guarded)

    with subtest("without it, systemd pets watchdog0, and the check says so"):
        assert armed(control) == ["Software Watchdog"], armed(control)
        out = control.fail("${check} 2>&1")
        assert "not i6300ESB timer" in out, out

    with subtest("each switch moves systemd onto the watchdog it names, and lets go of the last"):
        for spec, identity in (("pinned", "i6300ESB timer"), ("soft", "Software Watchdog"), ("pinned", "i6300ESB timer")):
            control.succeed(f"{base}/specialisation/{spec}/bin/switch-to-configuration test")
            retry(lambda _: checks(control).get("watchdog") == "ok")
            assert armed(control) == [identity], (spec, armed(control))

    with subtest("a switch off the option goes back to watchdog0"):
        control.succeed(f"{base}/bin/switch-to-configuration test")
        control.wait_until_succeeds(
            "grep -qx active /sys/class/watchdog/watchdog0/state"
            " && grep -qx inactive /sys/class/watchdog/watchdog1/state"
        )
        assert armed(control) == ["Software Watchdog"], armed(control)
        assert "watchdog" not in checks(control), checks(control)

    # Activation waits for the link while holding the lock the switch's revert
    # needs: a udev that never gets to it must fail the link, not hang there.
    with subtest("a stalled udev fails the link in bounded time"):
        control.succeed("udevadm control --stop-exec-queue")
        rc, out = control.execute(f"timeout 300 {base}/specialisation/pinned/bin/switch-to-configuration test 2>&1")
        control.succeed("udevadm control --start-exec-queue")
        assert rc != 124, "the switch hung on udev"
        assert "hardware-watchdog-link.service" in out, out
        control.succeed("systemctl restart hardware-watchdog-link")
        retry(lambda _: checks(control).get("watchdog") == "ok")
        assert armed(control) == ["i6300ESB timer"], armed(control)
  '';
}
