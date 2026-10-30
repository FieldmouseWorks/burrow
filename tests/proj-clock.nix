# A turn's clock, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that a turn whose command waits in line (a
# file it holds locked in the turn's PROJ_WAIT_DIR) runs past its --timeout
# while it waits, that `proj status` and the stall watchdog say what it waits
# for (the watchdog holding it to stallAfterMinutes from when it began), that
# its clock runs again once the wait is over, that a waiter that dies
# mid-wait, its file left behind, stops the clock for no longer than it
# lived, that a turn that never waits is due its slot's moment and its
# timeout, to the nanosecond, and that the watchdog's lock, held, holds up
# neither a turn's clock nor its end, nor leaves the watchdog a wait that's
# over when a run is cut short mid-wait. tests/proj.nix has the rest of
# proj, its plain timeouts included.
#
#   nix build .#checks.x86_64-linux.proj-clock -L
{ self, pkgs }:
let
  # Claude, as far as proj can tell: says it's done, then "wait N WHAT" waits
  # in line N seconds for WHAT, as a lock's waiter does (its file in
  # PROJ_WAIT_DIR, held locked while it waits, removed once it's through),
  # and answers; "waitwork N WHAT" does, then works on for good; "later N
  # WHAT" works ten seconds first; "die N WHAT" has a waiter that holds its
  # file N seconds and exits without removing it, then works on for good;
  # "cut N WHAT" leaves that waiter in the background, answers eight seconds
  # on, and stays up for it, so proj cuts the run short. Anything else
  # ("work", or proj's word that it cut the last run short) never waits, and
  # works on for good.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    proj done >/dev/null
    read -r how n what <<<"$prompt"
    result() { ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'; }
    case $how in
      wait | waitwork | die | cut) ;;
      later) sleep 10 ;;
      *) exec sleep 600 ;;
    esac
    f=$PROJ_WAIT_DIR/$$
    printf '%s\n' "$what" >"$f"
    case $how in
      wait | waitwork | later)
        exec 9<"$f"
        flock -x 9
        sleep "$n"
        rm -f "$f"
        exec 9<&-
        [ "$how" = wait ] || sleep 600 ;;
      die)
        ( exec 9<"$f" && flock -x 9 && exec sleep "$n" )
        sleep 600 ;;
      cut)
        ( exec 9<"$f" && flock -x 9 && exec sleep "$n" ) &
        sleep 8
        result
        wait
        exit 0 ;;
    esac
    result
  '';
in
pkgs.testers.runNixOSTest {
  name = "proj-clock";

  nodes.machine = {
    imports = [
      self.nixosModules.proj
      self.nixosModules.watchdog
    ];
    burrow = {
      claude.package = claude;
      # On, so proj keeps its waits; it never runs here (no burrow-events),
      # the test reads what proj writes.
      watchdog.enable = true;
      proj = {
        enable = true;
        minFreeMemory = 0;
        stallAfterMinutes = 30;
      };
    };
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [ "d /data/dev/src/web 0755 dev dev -" ];
  };

  testScript = ''
    import json
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def stamp(job, name):
        return float(machine.succeed(f"stat -c %.9Y /data/dev/jobs/{job}/{name}"))

    def took(job):
        # From its slot to its end.
        return stamp(job, "1.outcome") - stamp(job, "1.slot")

    def wait_of(job, holds, timeout=60):
        # The job's wait as proj last told the watchdog, once holds(it) does.
        deadline = time.monotonic() + timeout
        while True:
            w = json.loads(machine.succeed("cat /data/dev/jobs/.waits.json 2>/dev/null || echo '{\"items\": {}}'"))["items"].get(job)
            if w is not None and holds(w):
                return w
            assert time.monotonic() < deadline, w
            time.sleep(0.5)

    machine.wait_for_unit("user@1001.service")

    with subtest("a turn waiting in line runs past its timeout, and says what for"):
        a = proj('run --timeout 6s web "wait 20 the heavy-run lock (held by a gate)"')
        machine.wait_until_succeeds(
            f"su - dev -c 'proj status' | grep -F {a} | grep -F '[waiting in line: the heavy-run lock (held by a gate)]'",
            timeout=60)
        w = wait_of(a, lambda w: "waiting in line: the heavy-run lock (held by a gate)" in w["phase"])
        assert w["due"] == w["since"] + 30 * 60, w
        # Well past its six seconds, and still going.
        time.sleep(10)
        assert state(a) == "running", state(a)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=60)
        assert proj(f"log {a}") == "did wait 20 the heavy-run lock (held by a gate)", proj(f"log {a}")
        assert took(a) >= 20, took(a)

    with subtest("its clock runs again once the wait is over"):
        a = proj('run --timeout 6s web "waitwork 10 the gate lock"')
        wait_of(a, lambda w: "waiting in line: the gate lock" in w["phase"])
        # Through: running again, held to its deadline, moved on by the wait.
        w = wait_of(a, lambda w: w["phase"] == "turn 1 is running")
        assert w["due"] - w["since"] >= 6 + 10 + 600 - 2, w
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'timed out after 6s'", timeout=60)
        # Its wait, then the rest of its six seconds (the clock is read once
        # a second).
        assert 14 <= took(a) < 30, took(a)
        assert "waiting in line" not in proj("status")

    with subtest("a waiter that dies mid-wait stops the clock no longer than it lived"):
        a = proj('run --timeout 6s web "die 4 the heavy-run lock"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'timed out after 6s'", timeout=60)
        assert 8 <= took(a) < 20, took(a)

    with subtest("a turn that never waits is due its slot's moment and its timeout"):
        a = proj('run --timeout 6s web work')
        w = wait_of(a, lambda w: w["phase"] == "turn 1 is running")
        slot = machine.succeed(f"stat -c %.9Y /data/dev/jobs/{a}/1.slot").strip()
        deadline = int(machine.succeed(f"cat /data/dev/jobs/{a}/1.deadline"))
        assert deadline == int(slot.replace(".", "")) + 6 * 10**9, (slot, deadline)
        assert w["since"] == int(slot.split(".")[0]) and w["due"] == w["since"] + 6 + 600, w
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'timed out after 6s'", timeout=60)
        assert 6 <= took(a) < 12, took(a)

    with subtest("the watchdog's lock, held, holds up neither a turn's clock nor its end"):
        # Both running and heard of before the lock is taken, as starting a
        # turn tells the watchdog itself; each waits in line ten seconds on.
        a = proj('run --timeout 20s web "later 3 the gate lock"')
        b = proj('run --idle 15s web "later 600 the heavy-run lock"')
        for j in (a, b):
            wait_of(j, lambda w: w["phase"] == "turn 1 is running")
        machine.succeed("systemd-run --unit=hold-waits flock /data/dev/jobs/.waits.lock ${pkgs.coreutils}/bin/sleep 600")
        machine.wait_until_fails("flock -n /data/dev/jobs/.waits.lock true", timeout=10)
        # Its twenty seconds and three in line, less what the once-a-second
        # reading of the clock misses of the wait; quiet for fifteen, still
        # in line.
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'timed out after 20s'", timeout=60)
        assert 21 <= took(a) < 35, took(a)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x 'went quiet for 15s'", timeout=60)
        assert 15 <= took(b) < 25, took(b)
        machine.succeed("systemctl stop hold-waits")
        # And the watchdog hears of both ends once it's free.
        for j in (a, b):
            wait_of(j, lambda w: w["phase"].startswith("turn 1 ended"))

    with subtest("a run cut short mid-wait leaves the watchdog no wait that's over"):
        a = proj('run --timeout 5m web "cut 600 the gate lock"')
        wait_of(a, lambda w: "waiting in line: the gate lock" in w["phase"])
        # Held while the run is cut short and its leftovers stopped, telling
        # the watchdog its wait was over with them.
        machine.succeed("systemd-run --unit=hold-waits flock /data/dev/jobs/.waits.lock ${pkgs.coreutils}/bin/sleep 600")
        machine.wait_until_fails("flock -n /data/dev/jobs/.waits.lock true", timeout=10)
        machine.wait_until_succeeds(f"test -s /data/dev/jobs/{a}/1.cut", timeout=60)
        machine.succeed("systemctl stop hold-waits")
        wait_of(a, lambda w: w["phase"] == "turn 1 is running", timeout=30)
        proj(f"stop {a}")
  '';
}
