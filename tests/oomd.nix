# burrow.ops's systemd-oomd: a service eating memory past RAM and into swap is
# killed by oomd while the machine still answers. The control has NixOS's own
# oomd, which runs but watches nothing (remi on 2026-10-29, which thrashed
# until it stopped answering): there only the kernel's OOM killer steps in,
# once memory and swap are gone.
#
#   nix build .#checks.x86_64-linux.oomd -L
{ self, pkgs }:
let
  # 32 MiB of random bytes a second, kept: random so zram, or anything else,
  # can't make it small.
  hog = pkgs.writers.writePython3 "hog" { } ''
    import os
    import time

    kept = []
    while True:
        kept.append(os.urandom(32 << 20))
        time.sleep(1)
  '';
  machine = oomd: {
    imports = [ self.nixosModules.ops ];
    burrow.ops = { enable = true; inherit oomd; };
    services.openssh.enable = true;
    virtualisation.memorySize = 1024;
    virtualisation.diskSize = 2048;
    # Test VMs panic on an OOM kill by default; here the kernel's OOM killer
    # is what the control waits for, and what the guarded machine must never
    # need.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
    systemd.services.hog.serviceConfig.ExecStart = hog;
  };
in
pkgs.testers.runNixOSTest {
  name = "oomd";

  nodes.guarded = {
    imports = [ (machine true) ];
    # The same, with sshd started per connection: built, never booted.
    specialisation.socketed.configuration.services.openssh.startWhenNeeded = true;
  };
  nodes.control = machine false;

  testScript = ''
    def monitored(m, section):
        """The cgroups oomctl lists under one section: an unindented
        "Section:" line, then each cgroup's indented "Path: ..." line with its
        details indented below it."""
        paths, inside = set(), False
        for line in m.succeed("oomctl").splitlines():
            if not line[:1].isspace():
                inside = line.strip() == section + ":"
            elif inside and line.strip().startswith("Path: "):
                paths.add(line.strip().removeprefix("Path: "))
        return paths

    start_all()
    for m in (guarded, control):
        m.wait_for_unit("multi-user.target")
        # A test VM drops swapDevices for its own, so make the swap here.
        m.succeed("fallocate -l 1G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile")

    # Pressure on the root or user.slice is never watched: oomd would ignore a
    # user's units' opt-outs there (tests/remote-control-oomd.nix).
    with subtest("oomd watches swap on the root, and pressure on the system slice"):
        swap = monitored(guarded, "Swap Monitored CGroups")
        pressure = monitored(guarded, "Memory Pressure Monitored CGroups")
        assert swap == {"/"}, swap
        assert "/system.slice" in pressure and not pressure & {"/", "/user.slice"}, pressure
        assert monitored(control, "Swap Monitored CGroups") == set()
        assert monitored(control, "Memory Pressure Monitored CGroups") == set()

    with subtest("the ways in are never picked"):
        for unit in ("sshd", "sshd@probe"):
            guarded.succeed(f"systemctl show -p ManagedOOMPreference {unit} | grep -qx ManagedOOMPreference=omit")
        socketed = "/run/current-system/specialisation/socketed/etc/systemd/system"
        guarded.succeed(f"grep -qx ManagedOOMPreference=omit {socketed}/sshd@.service")
        guarded.fail(f"test -e {socketed}/sshd.service")

    # systemd says which killer it was: "systemd-oomd killed N process(es) in
    # this unit", or "The kernel OOM killer killed some processes in this unit".
    with subtest("oomd kills the hog, and the kernel never has to"):
        guarded.succeed("systemctl start hog")
        guarded.wait_until_succeeds("journalctl -u hog | grep -q 'systemd-oomd killed'", timeout=600)
        guarded.succeed("true")
        guarded.fail("journalctl -k | grep -q 'Out of memory'")

    with subtest("without it, nothing acts until the kernel's OOM killer"):
        control.succeed("systemctl start hog")
        control.wait_until_succeeds("journalctl -k | grep -q 'Out of memory: Killed process'", timeout=600)
        control.fail("journalctl -u hog | grep -q 'systemd-oomd killed'")
  '';
}
