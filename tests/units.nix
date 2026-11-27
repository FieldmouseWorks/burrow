# burrow-units, for real: its watcher follows the journal and asks for a
# pass each time a unit fails, a unit in its watch file recovers or a
# program dumps core (not each time any unit starts), touching no
# unit's own OnFailure= (a service whose failure isolates a target still loads).
# A unit that fails, and a program that dumps core, whoever ran it, land in
# its watch file and kick the watchdog (a stand-in here, noting each kick);
# a unit that runs again leaves it without a manual scan, and a crash
# once it's cleared, until the next. Only the service
# passes (a pass that can't read the whole journal is refused), so a clear
# by a user who can't keeps everyone else's crashes. A crash in a Nix build,
# or in a proj job's slice, is theirs and never lands. What an item says, and when it's due, are
# crates/burrow-units' tests.
#
#   nix build .#checks.x86_64-linux.units -L
{ self, pkgs }:
let
  watchdog = pkgs.writeShellScript "watchdog-stand-in" ''
    date +%s >>/var/lib/watchdog-kicks/kicks
  '';
  broken = pkgs.writeShellScript "broken" ''
    [ -e /run/broken-fixed ] || { echo "not yet"; exit 3; }
  '';
in
pkgs.testers.runNixOSTest {
  name = "units";

  nodes.machine = {
    imports = [ self.nixosModules.watchdog ];
    users.users.dev = { isNormalUser = true; group = "dev"; };
    users.groups.dev = { };
    burrow.events = {
      enable = true;
      host = "example.org";
      secretFile = "/etc/burrow-webhook";
      handlers.watchdog = {
        events = [ ];
        repos = [ ];
        perRepo = false;
        unit = "watchdog-stand-in";
      };
    };
    environment.etc."burrow-webhook".text = "hook-secret\n";
    burrow.watchdog.units.enable = true;
    # The hourly catch-up fires on the wall clock, so a run that straddles
    # the hour would see an unexpected full pass.
    systemd.timers.burrow-events-catch-up.enable = false;
    # proj's slice, without proj: a crash in a unit there is its own.
    burrow.watchdog.units.ownedSlices = [ "claude-jobs.slice" ];
    # The state lives under /home, through a symlink: the clear handler
    # must still reach it.
    systemd.tmpfiles.settings."00-burrow-home-state" = {
      "/home/dev/burrow-state".d = { mode = "0755"; user = "dev"; group = "dev"; };
      "/var/lib/burrow".L.argument = "/home/dev/burrow-state";
    };
    systemd.services.watchdog-stand-in.serviceConfig = {
      Type = "oneshot";
      ExecStart = "${watchdog}";
      StateDirectory = "watchdog-kicks";
    };
    systemd.services.broken.serviceConfig = {
      Type = "oneshot";
      ExecStart = "${broken}";
    };
    # A healthy unit, as a timer's service is every few seconds.
    systemd.services.fine.serviceConfig = {
      Type = "oneshot";
      ExecStart = "${pkgs.coreutils}/bin/true";
    };
    # Its failure isolates a target, which allows it no other OnFailure=.
    systemd.services.isolating = {
      onFailure = [ "rescue.target" ];
      unitConfig.OnFailureJobMode = "isolate";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/true";
      };
    };
  };

  testScript = { nodes, ... }: let scratch = nodes.machine.burrow.scratchDir; in ''
    import json

    watch_file = "/var/lib/burrow/units/watch.json"

    def watch():
        return json.loads(machine.succeed(f"cat {watch_file}"))["items"]

    def kicks():
        return len(machine.succeed("cat /var/lib/watchdog-kicks/kicks 2>/dev/null || true").split())

    def dev_crash():
        machine.succeed("su - dev -c 'ulimit -c unlimited; sh -c \"kill -SEGV \\$\\$\"' || true")

    # sleep, once it's exec'd: a program of its own, run by root.
    def root_crash():
        machine.succeed("bash -c 'ulimit -c unlimited; sleep 60 & p=$!; sleep 1; kill -SEGV $p; wait $p' || true")

    # Journal filters matching burrow-units-watch's Want::Pass entries
    # (UNIT_FAILURE_RESULT / UNIT_FAILED from pid 1, and systemd-coredump).
    pass_events = (
        "MESSAGE_ID=d9b373ed55a64feb8242e02dbe79a49c "
        "MESSAGE_ID=be02cf6855d2428ba40df7e9d022f03d _PID=1 + "
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1 "
        "MESSAGE_ID=1f4e0a44a88649939aaea34fc6da8c95"
    )

    def journal_entries(args, needle=None):
        out = machine.succeed(f"journalctl -b --quiet -o json --output-fields=MESSAGE {args}")
        entries = []
        for line in out.splitlines():
            if not line.strip():
                continue
            entry = json.loads(line)
            if needle is not None:
                msg = entry.get("MESSAGE", "")
                if not (isinstance(msg, str) and needle in msg):
                    continue
            entries.append(entry)
        return entries

    def last_ts(entries):
        return max((int(e["__REALTIME_TIMESTAMP"]) / 1e6 for e in entries if "__REALTIME_TIMESTAMP" in e), default=0.0)

    def asked():
        machine.succeed("journalctl --sync")
        return len(journal_entries("-u burrow-units-watch.service", "asked for a pass"))

    def wait_for_units_idle():
        # Wait until every prior failure/coredump has been asked for by the
        # watcher, any resulting pass in burrow-events (held up to its 20s
        # gap) has run, and burrow-units.service has exited.
        def idle(_last_try):
            machine.succeed("journalctl --sync")
            t_event = last_ts(journal_entries(pass_events))
            asks = journal_entries("-u burrow-units-watch.service", "asked for a pass")
            t_ask = last_ts(asks)
            if t_ask == 0.0 or t_ask < t_event:
                return False
            starts = [
                e for e in journal_entries("-u burrow-events.service")
                if e.get("MESSAGE") == "units: full pass"
            ]
            done = journal_entries("_PID=1 UNIT=burrow-units.service MESSAGE_ID=7ad2d189f7e94e70a38c781354912448")
            t_pass = last_ts(starts)
            if t_pass == 0.0 or last_ts(done) < t_pass:
                return False
            if machine.succeed("systemctl show -P ActiveState burrow-units.service").strip() != "inactive":
                return False
            if t_pass > t_ask + 1.0:
                return True
            if abs(t_pass - t_ask) <= 1.0:
                t_now = float(machine.succeed("date +%s.%N").strip())
                return t_now > t_pass + 25.0
            return False

        retry(idle, timeout_seconds=120)

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("burrow-events.service")
    machine.wait_for_open_port(8788)
    machine.wait_for_unit("burrow-units-watch.service")

    with subtest("no unit's own failure settings are touched"):
        assert machine.succeed("systemctl show -p LoadState --value isolating.service").strip() == "loaded"
        assert machine.succeed("systemctl show -p OnFailure --value isolating.service").strip() == "rescue.target"
        assert machine.succeed("systemctl show -p OnFailure --value broken.service").strip() == ""

    with subtest("a failed unit is in its watch file, and the watchdog is kicked"):
        # The watcher asks for a pass as it starts; wait for both startup passes.
        machine.wait_until_succeeds(f"test -e {watch_file}", timeout=120)
        machine.wait_until_succeeds("[ $(wc -l </var/lib/watchdog-kicks/kicks 2>/dev/null || echo 0) -ge 1 ]", timeout=60)
        before = kicks()
        machine.fail("systemctl start broken.service")
        machine.wait_until_succeeds(f"grep -q unit:broken.service {watch_file}", timeout=120)
        item = watch()["unit:broken.service"]
        assert item["phase"] == "failed (exit-code)", item
        assert item["due"] == item["since"] + 600, item
        machine.wait_until_succeeds(f"[ $(wc -l </var/lib/watchdog-kicks/kicks) -gt {before} ]", timeout=60)

    with subtest("a unit that wasn't failed starting and succeeding asks for no pass"):
        # What the failure set off (any ask queued behind burrow-events' gap) settles first.
        wait_for_units_idle()
        before, asks = json.loads(machine.succeed(f"cat {watch_file}"))["at"], asked()
        machine.succeed("systemctl start fine.service")
        machine.succeed("sleep 5")
        machine.succeed("systemctl start fine.service")
        machine.succeed("sleep 5")
        assert asked() == asks, (asks, asked())
        after = json.loads(machine.succeed(f"cat {watch_file}"))["at"]
        assert after == before, (before, after)

    with subtest("a program that dumps core is in it too, whoever ran it, but a build's, a job's or a work tree's"):
        # A build user's crash fails its build, and a job's is the job's:
        # logged first, so every pass after the two below reads them.
        machine.succeed("bash -c 'ulimit -c unlimited; setpriv --reuid=nixbld1 --regid=nixbld --clear-groups ${pkgs.dash}/bin/dash -c \"kill -SEGV \\$\\$\"' || true")
        machine.succeed("systemd-run --wait --slice=claude-jobs.slice -p LimitCORE=infinity ${pkgs.mksh}/bin/mksh -c 'kill -SEGV 0' || true")
        machine.wait_until_succeeds("coredumpctl list --no-pager ${pkgs.dash}/bin/dash", timeout=60)
        machine.wait_until_succeeds("coredumpctl list --no-pager ${pkgs.mksh}/bin/mksh", timeout=60)
        # And a program built in a work tree (a job's scratch) is whoever
        # ran it's, in no owned slice.
        machine.succeed("install -D -m 755 ${pkgs.dash}/bin/dash ${scratch}/build/dash && chown -R dev ${scratch}/build")
        machine.succeed("su - dev -c 'ulimit -c unlimited; ${scratch}/build/dash -c \"kill -SEGV \\$\\$\"' || true")
        machine.wait_until_succeeds("coredumpctl list --no-pager ${scratch}/build/dash", timeout=60)
        dev_crash()
        root_crash()
        machine.wait_until_succeeds(f"[ $(grep -o '\"core:[^\"]*\"' {watch_file} | sort -u | wc -l) -eq 2 ]", timeout=120)
        cores = [k for k in watch() if k.startswith("core:")]
        assert not any(k.endswith(("/bin/dash", "/bin/mksh", "/build/dash")) for k in cores), cores
        bash = next(k for k in cores if k.endswith("/bin/bash"))
        # sleep's own program: coreutils, where it's one binary for all.
        root = next(k for k in cores if k != bash)

    with subtest("an isolated caller clears host crashes, everyone else's stay, and a later one is back"):
        # dev can't read root's crashes itself, so they stay only if the
        # pass that follows its clear is the service's.
        assert root[len("core:"):] not in machine.succeed("su - dev -c 'coredumpctl list --no-pager --json=short' 2>&1 || true")
        # Any crash burst's pass queued in burrow-events finishes before we
        # assert a refused user pass leaves watch.json untouched.
        wait_for_units_idle()
        # And a pass of its own, which would drop them, it's refused.
        before = machine.succeed(f"cat {watch_file}")
        machine.fail("su - dev -c 'burrow-units pass'")
        assert machine.succeed(f"cat {watch_file}") == before
        exe = bash[len("core:"):]
        # The same empty-root boundary as a sandboxed job: system programs
        # and only the clear socket, with no host state or journals. The
        # host configuration is loaded by the socket-activated service.
        isolated = "${pkgs.bubblewrap}/bin/bwrap --ro-bind /nix/store /nix/store --ro-bind /run/current-system/sw /run/current-system/sw --ro-bind /run/burrow-units/clear.sock /run/burrow-units/clear.sock --proc /proc --dev /dev --tmpfs /tmp --"
        machine.succeed(f"su - dev -c '{isolated} /run/current-system/sw/bin/test ! -e /var/lib/burrow/units'")
        before = json.loads(machine.succeed(f"cat {watch_file}"))["at"]
        machine.succeed(f"su - dev -c '{isolated} /run/current-system/sw/bin/burrow-units clear {exe}'")
        acknowledged = json.loads(machine.succeed("cat /var/lib/burrow/units/cleared.json"))
        assert exe in acknowledged, acknowledged
        assert acknowledged[exe] >= before, acknowledged
        machine.wait_until_succeeds(f"! grep -qF '\"{bash}\"' {watch_file}", timeout=60)
        assert json.loads(machine.succeed(f"cat {watch_file}"))["at"] > before
        assert root in watch(), watch()
        machine.succeed("sleep 1")
        dev_crash()
        machine.wait_until_succeeds(f"grep -qF '\"{bash}\"' {watch_file}", timeout=120)
        assert "crashed (last:" in watch()[bash]["what"], watch()

    with subtest("a unit that recovers leaves without a manual pass"):
        before = kicks()
        machine.succeed("touch /run/broken-fixed && systemctl start broken.service")
        machine.wait_until_succeeds(f"! grep -q unit:broken.service {watch_file}", timeout=60)
        machine.wait_until_succeeds(f"[ $(wc -l </var/lib/watchdog-kicks/kicks) -gt {before} ]", timeout=60)
  '';
}
