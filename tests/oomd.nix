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
    swapDevices = [{ device = "/swapfile"; size = 1024; }];
    systemd.services.hog.serviceConfig.ExecStart = hog;
  };
in
pkgs.testers.runNixOSTest {
  name = "oomd";

  nodes.guarded = machine true;
  nodes.control = machine false;

  testScript = ''
    start_all()
    for m in (guarded, control):
        m.wait_for_unit("multi-user.target")
        m.wait_until_succeeds("swapon --show=NAME --noheadings | grep -q /swapfile")

    with subtest("the ways in are never picked"):
        guarded.succeed("systemctl show -p ManagedOOMPreference sshd | grep -qx ManagedOOMPreference=omit")
        guarded.succeed("oomctl | grep -A1 'Swap Monitored CGroups' | grep -q 'Path: /$'")
        guarded.succeed("oomctl | grep -A3 'Memory Pressure Monitored CGroups' | grep -q 'Path: /system.slice'")

    with subtest("oomd kills the hog, and the kernel never has to"):
        guarded.succeed("systemctl start hog")
        guarded.wait_until_succeeds("journalctl -u systemd-oomd | grep -q 'Killed /system.slice/hog.service'", timeout=600)
        guarded.succeed("true")
        guarded.fail("journalctl -k | grep -q 'Out of memory'")

    with subtest("without it, nothing acts until the kernel's OOM killer"):
        control.succeed("systemctl start hog")
        control.wait_until_succeeds("journalctl -k | grep -q 'Out of memory: Killed process'", timeout=600)
        control.fail("journalctl -u systemd-oomd | grep -q Killed")
  '';
}
