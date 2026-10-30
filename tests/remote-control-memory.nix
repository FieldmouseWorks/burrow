# Each Remote Control session has a memory cap of its own, and so does its
# server, so a session that grows past its own is killed and nothing else is:
# not the server, not its other sessions, however much they hold between
# them. Two controls: a server whose sessions share one cap (how it was)
# loses one of two sessions that each fit, and a server on systemd's own OOM
# policy is taken down whole by the same kill. Servers start on tmux's usual
# build, whose panes get away from their unit, and catch up onto their own
# once idle.
#
#   nix build .#checks.x86_64-linux.remote-control-memory -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
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
  # Each project's sessions: NAME sleeps, NAME:MIB waits for a line on the
  # folder's NAME.go, then holds MIB MiB.
  projects = {
    capped = [ "sibling" "hog:256" "one:80" "two:80" ];
    # The first control: its sessions share one cap.
    shared = [ "one:80" "two:80" ];
    # The second: systemd's own OOM policy.
    stops = [ "sibling" "hog:256" ];
    # The cap no server sets.
    plain = [ "sibling" ];
  };
  name = s: builtins.head (lib.splitString ":" s);
  names = builtins.mapAttrs (_: map name) projects;
  holders = builtins.mapAttrs (_: sessions: map name (builtins.filter (lib.hasInfix ":") sessions)) projects;
  # Claude's server, as far as the caps can tell: one process, with a child
  # for each session in its folder's `sessions`, and Claude Code's
  # ~/.claude/sessions/PID.json for it, which starts the scan that gives it
  # its cgroup. Each says it has been idle for hours, so none holds a stale
  # server back. The server notes the cgroup it started in (`started`), each
  # session's pid (NAME.pid) and how it ended (NAME.exit).
  claude = pkgs.writeShellScriptBin "claude" ''
    cat /proc/self/cgroup >>started
    mkdir -p ~/.claude/sessions
    declare -A names
    for s in $(cat sessions); do
      name=''${s%%:*}
      case $s in
        *:*) { read -r _ <"$name.go"; exec ${hog}/bin/hog "''${s#*:}"; } & ;;
        *) sleep infinity & ;;
      esac
      names[$!]=$name
      echo $! >"$name.pid"
      echo "{\"pid\":$!,\"status\":\"idle\",\"statusUpdatedAt\":$(( ($(${pkgs.coreutils}/bin/date +%s) - 7200) * 1000 ))}" >~/.claude/sessions/$!.json
    done
    while wait -n -p ended; status=$?; [ -n "''${ended:-}" ]; do
      echo $status >"''${names[$ended]}.exit"
      ended=
    done
    exec sleep infinity
  '';
in
pkgs.testers.runNixOSTest {
  name = "remote-control-memory";

  nodes.machine = {
    imports = [ self.nixosModules.remote-control ];
    virtualisation.memorySize = 2048;
    burrow = {
      claude.package = claude;
      remoteControl = {
        enable = true;
        # tmux's usual build: each server's pane, and so its sessions, in a
        # scope of their own.
        tmux = pkgs.tmux;
        servers = lib.genAttrs [ "capped" "shared" "stops" ] (_: { sessionMemoryMax = "128M"; });
      };
    };
    # The same machine with the servers' own tmux.
    specialisation.capped.configuration = { options, ... }: {
      burrow.remoteControl.tmux = lib.mkForce options.burrow.remoteControl.tmux.default;
    };
    systemd.user.services = {
      "claude-rc@shared" = {
        overrideStrategy = "asDropin";
        serviceConfig.MemoryMax = "128M";
      };
      "claude-rc@stops" = {
        overrideStrategy = "asDropin";
        serviceConfig.OOMPolicy = "stop";
      };
    };

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      # Logged in already, as far as claude-rc-serve checks.
      "f /home/dev/.claude.json 0600 dev dev - {}"
    ] ++ lib.concatLists (lib.mapAttrsToList (p: sessions: [
      "d /data/dev/src/${p} 0755 dev dev -"
      "f /data/dev/src/${p}/sessions 0644 dev dev - ${lib.concatStringsSep " " sessions}"
    ] ++ map (s: "p /data/dev/src/${p}/${s}.go 0600 dev dev -") holders.${p}) projects);
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import json
    import re

    SESSIONS = json.loads(${builtins.toJSON (builtins.toJSON names)})

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

    def group(project, name):
        # One of the groups under the server's cgroup.
        return f"/sys/fs/cgroup{show(project, 'ControlGroup')['ControlGroup']}/{name}"

    def server(project):
        # The stand-in's pid: the pane of the unit's tmux.
        return machine.succeed(f"pgrep -P {show(project, 'MainPID')['MainPID']}").strip()

    def rss_mib(p):
        return int(machine.succeed(f"awk '/^VmRSS:/ {{ print $2 }}' /proc/{p}/status")) // 1024

    def go(project, session):
        machine.succeed(f"echo >/data/dev/src/{project}/{session}.go")

    def logged(project, text):
        # What the user manager said about the server's unit.
        machine.wait_until_succeeds(f"journalctl -o cat USER_UNIT={unit(project)} | grep -qF \"{text}\"", timeout=60)

    def placed(project):
        # The server's tmux and its Claude in the server's own group, and
        # each session in one of its own, all under the unit's cgroup.
        u, d = unit(project), f"/data/dev/src/{project}"
        machine.wait_until_succeeds(
            f"main=$(systemctl --user -M dev@ show -p MainPID --value {u}) && "
            f"for p in $main $(pgrep -P $main); do grep -qx '0::/.*/{u}/server' /proc/$p/cgroup || exit 1; done && "
            f"for s in {' '.join(SESSIONS[project])}; do p=$(cat {d}/$s.pid); grep -qx \"0::/.*/{u}/session-$p\" /proc/$p/cgroup || exit 1; done",
            timeout=60)
        assert started(project)[-1].endswith(f"/{u}"), started(project)

    machine.wait_for_unit("user@1001.service")
    for p, names in SESSIONS.items():
        for s in names:
            machine.wait_until_succeeds(f"test -s /data/dev/src/{p}/{s}.pid", timeout=120)

    with subtest("on tmux's usual build, a server's sessions get away from its unit"):
        hog = pid("capped", "hog")
        assert re.fullmatch(r"0::/.*/tmux-spawn-[^/]+\.scope", cgroup(hog)), cgroup(hog)
        go("capped", "hog")
        retry(lambda _: rss_mib(hog) >= 256, timeout_seconds=60)
        machine.succeed(f"kill {hog}")

    old = [pid(p, s) for p, names in SESSIONS.items() for s in names]
    machine.succeed("/run/current-system/specialisation/capped/bin/switch-to-configuration test")

    with subtest("a server on another tmux restarts onto its own once idle, and each session gets a group of its own"):
        for p in SESSIONS:
            placed(p)
            assert len(started(p)) == 2, (p, started(p))
        for o in old:
            machine.wait_until_fails(f"kill -0 {o}", timeout=30)

    with subtest("every session has a cap of its own, and so does the server, none of it in swap"):
        unit_caps = show("capped", "MemoryMax", "MemorySwapMax", "OOMPolicy", "Delegate", "DelegateControllers")
        assert unit_caps == {"MemoryMax": "infinity", "MemorySwapMax": "0", "OOMPolicy": "continue",
                             "Delegate": "yes", "DelegateControllers": "memory"}, unit_caps
        assert show("stops", "OOMPolicy") == {"OOMPolicy": "stop"}, show("stops", "OOMPolicy")
        for p, cap in (("capped", 128 << 20), ("shared", 128 << 20), ("stops", 128 << 20), ("plain", 8 << 30)):
            for g in ["server"] + [f"session-{pid(p, s)}" for s in SESSIONS[p]]:
                got = machine.succeed(f"cat {group(p, g)}/memory.max").strip()
                assert got == str(cap), (p, g, got)

    with subtest("sessions that each fit their cap carry on, whatever they hold between them"):
        both = [pid("capped", s) for s in ("one", "two")]
        for s in ("one", "two"):
            go("capped", s)
        for p in both:
            retry(lambda _, p=p: rss_mib(p) >= 80, timeout_seconds=60)
        held = sum(int(machine.succeed(f"cat {group('capped', f'session-{p}')}/memory.current")) for p in both)
        assert held > 128 << 20, held
        assert all(alive(p) for p in both), [alive(p) for p in both]

    with subtest("the control: sharing one cap, the same two lose one"):
        both = {s: pid("shared", s) for s in ("one", "two")}
        for s in both:
            go("shared", s)
        machine.wait_until_succeeds("grep -qsx 137 /data/dev/src/shared/one.exit /data/dev/src/shared/two.exit", timeout=60)
        killed = [s for s in both if machine.execute(f"grep -qx 137 /data/dev/src/shared/{s}.exit")[0] == 0]
        assert len(killed) == 1, killed
        assert [s for s, p in both.items() if alive(p)] == [s for s in both if s not in killed]

    with subtest("a session past its own cap is killed, and nothing else is"):
        others = [pid("capped", s) for s in ("sibling", "one", "two")] + [server("capped")]
        state = ("MainPID", "InvocationID", "NRestarts", "ActiveState")
        before = show("capped", *state)
        assert before["ActiveState"] == "active", before
        go("capped", "hog")
        machine.wait_until_succeeds("grep -qx 137 /data/dev/src/capped/hog.exit", timeout=60)
        events = machine.succeed(f"cat {group('capped', '.')}/memory.events")
        assert re.search(r"^oom_kill 1$", events, re.M), events
        # systemd has seen the kill, and acted on it as it was going to.
        logged("capped", "The kernel OOM killer killed some processes in this unit.")
        assert all(alive(p) for p in others), [alive(p) for p in others]
        assert show("capped", *state) == before, show("capped", *state)

    with subtest("the control: on systemd's own OOM policy, the same kill takes the whole server down"):
        sibling = pid("stops", "sibling")
        restarts = int(show("stops", "NRestarts")["NRestarts"])
        go("stops", "hog")
        logged("stops", "Failed with result 'oom-kill'.")
        machine.wait_until_fails(f"kill -0 {sibling}", timeout=60)
        machine.wait_until_succeeds(f"[ $(systemctl --user -M dev@ show -p NRestarts --value {unit('stops')}) -gt {restarts} ]", timeout=60)
        placed("stops")
  '';
}
