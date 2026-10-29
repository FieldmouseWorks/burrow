# burrow.ops.watchdogIdentity: with two watchdogs and the wrong one first as
# /dev/watchdog0 (softdog loads in the initrd, the i6300ESB after it), systemd
# pets the named one, and HOST-health's watchdog check passes. The control,
# without the option, pets /dev/watchdog0, softdog (remi on 2026-10-29, whose
# systemd petted the overclocking watchdog rather than iTCO), and the same
# check, run there, fails, until it switches to a system with the option, as
# a deploy would.
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
  };

  testScript = { nodes, ... }: let
    # The guarded machine's check, to run on either.
    check = pkgs.writeShellScript "health-watchdog" ''
      export PATH=${pkgs.lib.makeBinPath [ pkgs.coreutils pkgs.systemd ]}
      ${nodes.guarded.burrow.updates.health.watchdog}
    '';
  in ''
    def armed(m):
        """The identities of the watchdogs that are armed: systemd is the only
        thing here that opens one."""
        return m.succeed(
            "for w in /sys/class/watchdog/watchdog*; do"
            " if grep -qx active $w/state; then cat $w/identity; fi; done"
        ).splitlines()

    start_all()
    for m in (guarded, control):
        m.wait_for_unit("multi-user.target")
        # Both watchdogs are there, softdog first.
        m.succeed("grep -qx 'Software Watchdog' /sys/class/watchdog/watchdog0/identity")
        m.succeed("grep -qx 'i6300ESB timer' /sys/class/watchdog/watchdog1/identity")

    with subtest("systemd pets the named watchdog, not watchdog0"):
        guarded.wait_until_succeeds("grep -qx active /sys/class/watchdog/watchdog1/state")
        assert armed(guarded) == ["i6300ESB timer"], armed(guarded)
        guarded.succeed("${check}")

    with subtest("without it, systemd pets watchdog0, and the check says so"):
        assert armed(control) == ["Software Watchdog"], armed(control)
        out = control.fail("${check} 2>&1")
        assert "not i6300ESB timer" in out, out

    with subtest("a switch to it moves systemd onto the named watchdog"):
        control.succeed("/run/current-system/specialisation/pinned/bin/switch-to-configuration test")
        control.wait_until_succeeds("${check}")
        assert armed(control) == ["i6300ESB timer"], armed(control)
  '';
}
