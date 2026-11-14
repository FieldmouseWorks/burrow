# A Remote Control server that dies at once restarts as its unit's RestartSec
# paces it, not as claude-rc-scan does: a scan leaves a unit waiting in
# auto-restart alone, so a run of crashing servers, each touching
# ~/.claude/sessions and so starting the scan again, is no restart storm.
#
#   nix build .#checks.x86_64-linux.remote-control-crash -L
{ self, pkgs }:
let
  # Claude's server, dying as soon as it starts; each start is noted.
  claude = pkgs.writeShellScriptBin "claude" ''
    echo start >>started
    exit 1
  '';
in
pkgs.testers.runNixOSTest {
  name = "remote-control-crash";

  nodes.machine = {
    imports = [
      self.nixosModules.remote-control
      (import ./remote-control/projects.nix { inherit pkgs; names = [ "crashing" ]; })
    ];
    burrow = {
      claude.package = claude;
      remoteControl.enable = true;
    };
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "f /home/dev/.claude.json 0600 dev dev - {}"
      "d /data/dev/src/crashing 0755 dev dev -"
    ];
  };

  testScript = ''
    def starts():
        return int(machine.succeed("wc -l </data/dev/src/crashing/started").strip())

    machine.wait_for_unit("user@1001.service")
    machine.wait_until_succeeds("test -s /data/dev/src/crashing/started", timeout=120)
    # Scans, one after another, as dying servers would make them.
    for _ in range(20):
        machine.succeed("su dev -c 'mkdir -p ~/.claude/sessions && touch ~/.claude/sessions/x.json && rm ~/.claude/sessions/x.json'")
        machine.sleep(1)
    n = starts()
    # RestartSec is 15s: over 20s and a bit more, the first start and one or
    # two restarts. A scan restarting it would make it dozens.
    assert n <= 4, f"{n} starts in about 20 seconds"
  '';
}
