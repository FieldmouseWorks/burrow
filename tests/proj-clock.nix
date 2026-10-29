# A turn's clock, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that a turn whose command waits in line (a
# file it holds locked in the turn's PROJ_WAIT_DIR) runs past its --timeout
# while it waits, that `proj status` and the stall watchdog say what it waits
# for (the watchdog holding it to stallAfterMinutes from when it began), that
# its clock runs again once the wait is over, and that a waiter that dies
# mid-wait, its file left behind, stops the clock for no longer than it
# lived. tests/proj.nix has the rest of proj, its plain timeouts included.
#
#   nix build .#checks.x86_64-linux.proj-clock -L
{ self, pkgs }:
let
  # Claude, as far as proj can tell: says it's done, then "wait N WHAT" waits
  # in line N seconds for WHAT, as a lock's waiter does (its file in
  # PROJ_WAIT_DIR, held locked while it waits, removed once it's through),
  # and answers; "waitwork N WHAT" does, then works on for good; "die N
  # WHAT" has a waiter that holds its file N seconds and exits without
  # removing it, then works on for good.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    proj done >/dev/null
    read -r how n what <<<"$prompt"
    f=$PROJ_WAIT_DIR/$$
    printf '%s\n' "$what" >"$f"
    case $how in
      wait | waitwork)
        exec 9<"$f"
        flock -x 9
        sleep "$n"
        rm -f "$f"
        exec 9<&-
        [ "$how" = wait ] || sleep 600 ;;
      die)
        ( exec 9<"$f" && flock -x 9 && exec sleep "$n" )
        sleep 600 ;;
    esac
    ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'
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
  '';
}
