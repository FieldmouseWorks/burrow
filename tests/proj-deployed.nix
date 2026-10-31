# A switch reaches the turns already waiting for a slot: at its next look each
# one decides by the deployed proj, in the same place in line and on the same
# clock towards maxWait. A proj that can't take the hand-over is left alone,
# and no look runs during one. A turn already running tells the stall watchdog
# by the deployed proj too.
# The first deployed here has one slot, the next two.
#
#   nix build .#checks.x86_64-linux.proj-deployed -L
{ self, pkgs }:
let
  inherit (import ./proj/stand-ins.nix { inherit pkgs; }) claude;
in
pkgs.testers.runNixOSTest {
  name = "proj-deployed";

  nodes.machine = { config, lib, ... }: {
    imports = [ self.nixosModules.proj self.nixosModules.watchdog ];
    burrow = {
      claude.package = claude;
      # On, so proj keeps its waits; it never runs here, the test reads them.
      watchdog.enable = true;
      proj = {
        enable = true;
        maxJobs = lib.mkDefault 1;
        pipelineSlots = 0;
        maxJobsPerProject = 1;
        minFreeMemory = 0;
        maxWait = 120;
      };
    };
    environment.systemPackages = [ pkgs.jq ];
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /data/dev/src/web 0755 dev dev -"
      "d /data/dev/src/ib 0755 dev dev -"
      "d /data/dev/src/bt 0755 dev dev -"
    ];
    specialisation.next.configuration.burrow.proj = {
      maxJobs = 2;
      stallAfterMinutes = 240;
    };
    # A proj from before the hand-over: any command but its own is refused,
    # and one that's run anyway says so.
    # A copy of the real one that says, first thing on a hand-over, whether a
    # look could run: the look's lock free as it comes up would let one clear
    # the place of a turn whose cmdline is empty mid-exec. A copy, so it's the
    # proj it runs as and the waiters settle on it.
    specialisation.handing.configuration.environment.systemPackages = [
      (lib.hiPrio (pkgs.runCommand "proj-handing" { } ''
        mkdir -p $out/bin
        sed '1a if [ "''${1:-}" = _exec ]; then if ${pkgs.util-linux}/bin/flock -n /data/dev/jobs/.slots/.lock true; then touch /tmp/unlocked; else touch /tmp/locked; fi; fi' \
          ${config.burrow.tools.proj}/bin/proj >$out/bin/proj
        chmod +x $out/bin/proj
      ''))
    ];
    specialisation.older.configuration.environment.systemPackages = [
      (lib.hiPrio (pkgs.writeShellScriptBin "proj" ''
        case ''${1:-} in _exec) touch /tmp/older-ran-exec; sleep 600 ;; esac
        echo "usage: proj run|reply" >&2
        exit 2
      ''))
    ];
  };

  testScript = ''
    import json
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def place(job):
        # Its place in line: the file (WHEN-ID, its prefix apart), and the pid.
        name = machine.succeed(f"ls /data/dev/jobs/.queue | grep -- '-{job}$'").strip()
        return name.split("-", 1)[1], machine.succeed(f"cat /data/dev/jobs/.queue/{name}").strip()

    def runs(pid):
        # The proj a waiter is running now, which its wrapper says.
        return machine.succeed(f"tr '\\0' '\\n' </proc/{pid}/environ | sed -n 's/^PROJ_SELF=//p'").strip()

    def deploy(name):
        machine.succeed(f"{base}/specialisation/{name}/bin/switch-to-configuration test")

    machine.wait_for_unit("user@1001.service")
    base = machine.succeed("readlink -f /run/current-system").strip()
    first = machine.succeed("readlink -f /run/current-system/sw/bin/proj").strip()

    a = proj('run web "sleep 600"')
    machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x running", timeout=120)
    q = proj('run ib "sleep 600"')
    r = proj('run bt "sleep 600"')
    machine.wait_until_succeeds(f"ls /data/dev/jobs/.queue | grep -q -- '-{r}$'", timeout=60)
    assert state(q) == "queued" and state(r) == "queued"
    in_line = {j: place(j) for j in (q, r)}
    assert runs(in_line[r][1]) == first, runs(in_line[r][1])
    queued = time.monotonic()

    with subtest("a proj that can't take the hand-over gets none: the turns stay on theirs, in place"):
        deploy("older")
        time.sleep(6)
        machine.fail("test -e /tmp/older-ran-exec")
        for j in (q, r):
            assert place(j) == in_line[j], (place(j), in_line[j])
            assert runs(in_line[j][1]) == first, runs(in_line[j][1])

    with subtest("no look runs while a turn is handed over, so none clears its place"):
        deploy("handing")
        handing = machine.succeed("readlink -f /run/current-system/sw/bin/proj").strip()
        assert handing != first
        for j in (q, r):
            machine.wait_until_succeeds(f"tr '\\0' '\\n' </proc/{in_line[j][1]}/environ | grep -Fx PROJ_SELF={handing}", timeout=30)
        machine.succeed("test -e /tmp/locked")
        machine.fail("test -e /tmp/unlocked")
        # Settled there, each handed over once: a look's worth of waiting later
        # the marker hasn't been written again, and the place is the same.
        machine.succeed("rm /tmp/locked")
        time.sleep(6)
        machine.fail("test -e /tmp/locked")
        for j in (q, r):
            assert place(j) == in_line[j], (place(j), in_line[j])

    with subtest("a turn already waiting follows the deployed proj: the second slot goes to the head of the line"):
        # Far enough into maxWait that a clock started again here would show.
        time.sleep(max(0, 30 - (time.monotonic() - queued)))
        handed = time.monotonic()
        deploy("next")
        machine.fail("test -e /tmp/unlocked")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x running", timeout=30)
        assert machine.succeed(f"cat /data/dev/jobs/{q}/1.slot").strip() == "2"
        next_proj = machine.succeed("readlink -f /run/current-system/sw/bin/proj").strip()
        assert next_proj != first

    with subtest("the turn behind it runs the deployed proj too, in the same place in line"):
        assert state(r) == "queued", state(r)
        machine.wait_until_succeeds(f"tr '\\0' '\\n' </proc/{in_line[r][1]}/environ | grep -Fx PROJ_SELF={next_proj}", timeout=30)
        assert place(r) == in_line[r], (place(r), in_line[r])

    with subtest("its clock towards maxWait didn't start again"):
        # maxWait from when it came, at least 30s before this hand-over: a clock
        # started again here would run all of it from the hand-over.
        machine.wait_until_succeeds(f"ls /data/dev/jobs/.queue | grep -q -- '^1-.*-{r}$'", timeout=125)
        assert time.monotonic() - handed < 105, time.monotonic() - handed
        assert place(r) == in_line[r], (place(r), in_line[r])

    with subtest("a turn already running tells the watchdog by the deployed proj"):
        # Its command waiting in line: watch_run republishes, due by the
        # stallAfterMinutes of what's deployed, not of what the turn runs.
        machine.execute(f"su - dev -c 'cd /data/dev/jobs/{a}/1.waits && echo a lock >held && flock held sleep 20' >/dev/null 2>&1 &")
        machine.wait_until_succeeds(f"jq -e '.items[\"{a}\"].phase | test(\"waiting in line\")' /data/dev/jobs/.waits.json", timeout=30)
        item = json.loads(machine.succeed(f"jq -c '.items[\"{a}\"]' /data/dev/jobs/.waits.json"))
        assert item["due"] - item["since"] == 240 * 60, item

    with subtest("and it takes the next slot on the new proj"):
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {r}' | grep -x running", timeout=30)
        assert machine.succeed(f"cat /data/dev/jobs/{r}/1.slot").strip() == "1"
        machine.fail(f"ls /data/dev/jobs/.queue | grep -- '-{r}$'")
        proj(f"stop {q}")
        proj(f"stop {r}")
  '';
}
