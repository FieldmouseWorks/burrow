# A Remote Control server never restarts on a switch (the operator has to live
# through the switch it confirms), so claude-rc-scan catches servers up: one
# still on an old claude-rc-serve (its Claude Code, its permission mode)
# restarts as soon as it holds no session, and not before.
#
#   nix build .#checks.x86_64-linux.remote-control -L
{ self, pkgs }:
let
  # Claude's server, as far as claude-rc-scan can tell: one process, with a
  # child for each session. A project folder with a `session` file holds one.
  # Each start notes which Claude it was in the folder's `started`.
  claude = tag: pkgs.writeShellScriptBin "claude" ''
    echo ${tag} >>started
    if [ -e session ]; then sleep 99999 & fi
    exec sleep infinity
  '';
in
pkgs.testers.runNixOSTest {
  name = "remote-control";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.remote-control ];
    burrow = {
      claude.package = claude "old";
      remoteControl.enable = true;
    };
    # The same machine with a newer Claude Code: a new claude-rc-serve.
    specialisation.next.configuration.burrow.claude.package = lib.mkForce (claude "new");

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      # Logged in already, as far as claude-rc-serve checks.
      "f /home/dev/.claude.json 0600 dev dev - {}"
      "d /data/dev/src/idle 0755 dev dev -"
      "d /data/dev/src/busy 0755 dev dev -"
      "f /data/dev/src/busy/session 0644 dev dev -"
    ];
  };

  testScript = ''
    import time

    def started(project):
        return machine.succeed(f"cat /data/dev/src/{project}/started").split()

    def scan():
        machine.succeed("systemctl --user -M dev@ start claude-rc-scan.service")

    machine.wait_for_unit("user@1001.service")
    for p in ("idle", "busy"):
        machine.wait_until_succeeds(f"grep -qx old /data/dev/src/{p}/started", timeout=120)

    machine.succeed("/run/current-system/specialisation/next/bin/switch-to-configuration test")

    with subtest("a stale server with no session restarts on the new one"):
        # The switch's own user activation may have run the scan already.
        scan()
        machine.wait_until_succeeds("grep -qx new /data/dev/src/idle/started", timeout=60)

    with subtest("a stale server holding a session waits"):
        time.sleep(5)
        assert started("busy") == ["old"], started("busy")

    with subtest("and restarts once the session is gone"):
        machine.succeed("pkill -f 'sleep 99999'")
        scan()
        machine.wait_until_succeeds("grep -qx new /data/dev/src/busy/started", timeout=60)

    with subtest("a server already on the new one stays up"):
        scan()
        time.sleep(5)
        assert started("idle") == ["old", "new"], started("idle")
  '';
}
