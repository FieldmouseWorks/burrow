# A Remote Control server that dies at once restarts as its unit's RestartSec
# paces it, not as claude-rc-scan does: a scan leaves a unit waiting in
# auto-restart alone, so a run of crashing servers, each touching
# ~/.claude/sessions and so starting the scan again, is no restart storm.
#
#   nix build .#checks.x86_64-linux.remote-control-crash -L
{ self, pkgs }:
let
  # Claude's server, dying as soon as it starts; each start is noted with
  # the second it came at.
  claude = pkgs.writeShellScriptBin "claude" ''
    ${pkgs.coreutils}/bin/date +%s >>started
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
    restart_sec = 15

    machine.wait_for_unit("user@1001.service")
    machine.wait_until_succeeds("test -s /data/dev/src/crashing/started", timeout=120)
    # Scans, one after another, as dying servers would make them.
    for _ in range(20):
        machine.succeed("su dev -c 'mkdir -p ~/.claude/sessions && touch ~/.claude/sessions/x.json && rm ~/.claude/sessions/x.json'")
        machine.sleep(1)
    # The window is measured, from the first start to now, on the machine's
    # own clock: a loaded VM's rounds can take far longer than 20s, and
    # RestartSec alone allows a restart every 15s of it. Paced by RestartSec,
    # that's the first start and one per 15s, give or take one a scan caught
    # between the server's death and its auto-restart; the slack is that and
    # a second's rounding. A scan restarting it at once would make at least
    # one a round, 20 in all, past this unless the rounds took minutes.
    first, n, now = (int(x) for x in machine.succeed(
        "f=/data/dev/src/crashing/started; head -n1 $f; wc -l <$f; date +%s"
    ).split())
    elapsed = now - first
    allowed = 1 + elapsed // restart_sec + 2
    assert n <= allowed, f"{n} starts in {elapsed} seconds, RestartSec {restart_sec} allows {allowed}"
  '';
}
