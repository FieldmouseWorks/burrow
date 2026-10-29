# A Remote Control server and its sessions share one memory cap, so a session
# that grows past it is killed and nothing else is: not the server, not its
# other sessions. The control is a server on systemd's own OOM policy, which
# the same kill takes down whole. Servers start on tmux's usual build, whose
# panes get away from their unit's cap, and catch up onto their own once idle.
#
#   nix build .#checks.x86_64-linux.remote-control-memory -L
{ self, pkgs }:
let
  # Holds MiB of memory until it's killed.
  hog = pkgs.writeCBin "hog" ''
    #include <stdlib.h>
    #include <string.h>
    #include <unistd.h>
    int main(int argc, char **argv) {
      for (long i = 0; i < atol(argv[1]); i++) {
        char *p = malloc(1 << 20);
        if (!p) return 1;
        memset(p, 1, 1 << 20);
      }
      pause();
      return 0;
    }
  '';
  # Claude's server, as far as the cap can tell: one process, with a child for
  # each session its folder's `sessions` names. A sibling sleeps; the hog waits
  # for a line on the folder's `go`, then holds 256M. Each says it has been
  # idle for hours, so none holds a stale server back. The server notes the
  # cgroup it started in (`started`), each session's pid (NAME.pid) and how it
  # ended (NAME.exit).
  claude = pkgs.writeShellScriptBin "claude" ''
    cat /proc/self/cgroup >>started
    mkdir -p ~/.claude/sessions
    declare -A names
    for s in $(cat sessions); do
      case $s in
        hog) { read -r _ <go; exec ${hog}/bin/hog 256; } & ;;
        *) sleep infinity & ;;
      esac
      names[$!]=$s
      echo $! >$s.pid
      echo "{\"pid\":$!,\"status\":\"idle\",\"statusUpdatedAt\":$(( ($(${pkgs.coreutils}/bin/date +%s) - 7200) * 1000 ))}" >~/.claude/sessions/$!.json
    done
    while wait -n -p ended; status=$?; [ -n "''${ended:-}" ]; do
      echo $status >"''${names[$ended]}.exit"
      ended=
    done
    exec sleep infinity
  '';
  projects = [ "capped" "stops" ];
in
pkgs.testers.runNixOSTest {
  name = "remote-control-memory";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.remote-control ];
    burrow = {
      claude.package = claude;
      remoteControl = {
        enable = true;
        # tmux's usual build: each server's pane, and so its sessions, in a
        # scope of their own.
        tmux = pkgs.tmux;
        servers = lib.genAttrs projects (_: { memoryMax = "128M"; });
      };
    };
    # The same machine with the servers' own tmux.
    specialisation.capped.configuration = { options, ... }: {
      burrow.remoteControl.tmux = lib.mkForce options.burrow.remoteControl.tmux.default;
    };
    # The control: systemd's own OOM policy.
    systemd.user.services."claude-rc@stops".serviceConfig.OOMPolicy = "stop";

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      # Logged in already, as far as claude-rc-serve checks.
      "f /home/dev/.claude.json 0600 dev dev - {}"
    ] ++ lib.concatMap (p: [
      "d /data/dev/src/${p} 0755 dev dev -"
      "f /data/dev/src/${p}/sessions 0644 dev dev - sibling hog"
      "p /data/dev/src/${p}/go 0600 dev dev -"
    ]) projects;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import re

    def unit(project):
        return f"claude-rc@{project}.service"

    def show(project, *props):
        out = machine.succeed(f"systemctl --user -M dev@ show {unit(project)} " + " ".join(f"-p {p}" for p in props))
        return dict(line.split("=", 1) for line in out.splitlines())

    def started(project):
        return machine.succeed(f"cat /data/dev/src/{project}/started").splitlines()

    def pid(project, session):
        return machine.succeed(f"cat /data/dev/src/{project}/{session}.pid").strip()

    def alive(p):
        return machine.execute(f"kill -0 {p}")[0] == 0

    def cgroup(p):
        return machine.succeed(f"cat /proc/{p}/cgroup").strip()

    def server(project):
        # The stand-in's pid: the pane of the unit's tmux.
        return machine.succeed(f"pgrep -P {show(project, 'MainPID')['MainPID']}").strip()

    def rss_mib(p):
        return int(machine.succeed(f"awk '/^VmRSS:/ {{ print $2 }}' /proc/{p}/status")) // 1024

    def logged(project, text):
        # What the user manager said about the server's unit.
        machine.wait_until_succeeds(f"journalctl -o cat USER_UNIT={unit(project)} | grep -qF \"{text}\"", timeout=60)

    def in_unit(project):
        # The server's sessions, and the server itself, all in its unit.
        machine.wait_until_succeeds(
            f"for s in hog sibling; do grep -qx '0::/.*/{unit(project)}' /proc/$(cat /data/dev/src/{project}/$s.pid)/cgroup || exit 1; done",
            timeout=60)
        assert started(project)[-1].endswith(f"/{unit(project)}"), started(project)

    machine.wait_for_unit("user@1001.service")
    for p in ("capped", "stops"):
        machine.wait_until_succeeds(f"test -s /data/dev/src/{p}/hog.pid && test -s /data/dev/src/{p}/sibling.pid", timeout=120)

    with subtest("every server has a cap of its own, none of it in swap"):
        assert show("plain", "MemoryMax", "MemorySwapMax", "OOMPolicy") == {
            "MemoryMax": str(8 << 30), "MemorySwapMax": "0", "OOMPolicy": "continue"}, show("plain", "MemoryMax", "MemorySwapMax", "OOMPolicy")
        assert show("capped", "MemoryMax", "OOMPolicy") == {"MemoryMax": str(128 << 20), "OOMPolicy": "continue"}, show("capped", "MemoryMax", "OOMPolicy")
        assert show("stops", "OOMPolicy") == {"OOMPolicy": "stop"}, show("stops", "OOMPolicy")

    with subtest("on tmux's usual build, a server's sessions get away from its cap"):
        hog = pid("capped", "hog")
        assert re.fullmatch(r"0::/.*/tmux-spawn-[^/]+\.scope", cgroup(hog)), cgroup(hog)
        machine.succeed("echo >/data/dev/src/capped/go")
        retry(lambda _: rss_mib(hog) >= 256, timeout_seconds=60)
        machine.succeed(f"kill {hog}")

    old = [pid(p, s) for p in ("capped", "stops") for s in ("hog", "sibling")]
    machine.succeed("/run/current-system/specialisation/capped/bin/switch-to-configuration test")

    with subtest("a server on another tmux restarts onto its own once idle, and takes its sessions along"):
        for p in ("capped", "stops"):
            in_unit(p)
            assert len(started(p)) == 2, started(p)
        for o in old:
            machine.wait_until_fails(f"kill -0 {o}", timeout=30)

    with subtest("a session past its server's cap is killed, and nothing else is"):
        sibling, srv = pid("capped", "sibling"), server("capped")
        state = ("MainPID", "InvocationID", "NRestarts", "ActiveState")
        before = show("capped", *state)
        assert before["ActiveState"] == "active", before
        machine.succeed("echo >/data/dev/src/capped/go")
        machine.wait_until_succeeds("grep -qx 137 /data/dev/src/capped/hog.exit", timeout=60)
        events = machine.succeed(f"cat /sys/fs/cgroup{show('capped', 'ControlGroup')['ControlGroup']}/memory.events")
        assert re.search(r"^oom_kill [1-9]", events, re.M), events
        # systemd has seen the kill, and acted on it as it was going to.
        logged("capped", "The kernel OOM killer killed some processes in this unit.")
        assert alive(sibling) and alive(srv), (alive(sibling), alive(srv))
        assert show("capped", *state) == before, show("capped", *state)

    with subtest("the control: on systemd's own OOM policy, the same kill takes the whole server down"):
        sibling = pid("stops", "sibling")
        restarts = int(show("stops", "NRestarts")["NRestarts"])
        machine.succeed("echo >/data/dev/src/stops/go")
        logged("stops", "Failed with result 'oom-kill'.")
        machine.wait_until_fails(f"kill -0 {sibling}", timeout=60)
        machine.wait_until_succeeds(f"[ $(systemctl --user -M dev@ show -p NRestarts --value {unit('stops')}) -gt {restarts} ]", timeout=60)
        in_unit("stops")
  '';
}
