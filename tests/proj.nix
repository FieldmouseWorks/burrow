# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead but can't starve a turn that has waited long, that a
# pipeline job's every turn, in one of the pipeline's own repos, takes the
# slot kept for them when the rest are full, and one in a lead repo goes
# ahead of the others, while one anywhere else goes as --first and says so
# (pick.sh's own check has how origins are told apart), that a job run
# --unstick takes a slot of its own, every turn of it, while every other one
# is held and turns wait in line, and waits only for another --unstick turn
# (one made so later, by proj unstick, too: waiting, or at its next turn),
# that proj withdraw ends a job only while its turn waits for a slot (one
# that has started, or ended, it leaves be, saying so), and that a turn takes
# a free slot only once its job's lock, which withdraw looks under, is free,
# that a job whose turn has ended frees its slot, that one stopped after
# its answer with work in the background has the rest of that run stopped,
# hears so once and answers again (the turn ending as its last run did, its
# log saying so even when its unit went first, and within one timeout that
# the clearing up between runs counts against), that a turn kicks what it
# was started to once it takes its slot (not while it waits in line) and
# again at its end, that a job's own --timeout stops its turns, and its
# --idle one that has gone quiet (and either still kicks), that a reply
# while the last turn's unit is still ending waits for it before its turn is
# written, that every turn says what comes next (proj await, ask or done), one
# that doesn't hearing so once and, still silent, being the stall watchdog's
# at once (as one that ended in a failure is, a turn in line or running to its
# deadline, and an await to its own, the watchdog kicked as they change),
# while a question, a finished job, a stopped one and an --owned job's end
# aren't, that a job run --app acts as its App every turn, whatever the login
# sets and if the App goes while it waits, or not at all, and that a job
# past its memory cap is killed by its own cgroup's limit rather than the
# machine running out, that what's killed is the command that outgrew it,
# the turn carrying on to its answer, and that a turn killed before it
# answers can still be continued, in its own session. Its GitHub side, proj
# await, proj-watch and proj-inbox, is proj-github.nix's part of the same
# script.
#
#   nix build .#checks.x86_64-linux.proj -L
{ self, pkgs }:
let
  inherit (import ./proj/stand-ins.nix { inherit pkgs; }) claude gh;
  # autoland's lists of the pipeline's own repos and its lead ones, which
  # proj reads, without the rest of autoland: web is one of the first here,
  # and lead the only one of the second.
  pipelineRepos = {
    options.burrow.autoland = {
      pipelineRepos = pkgs.lib.mkOption { type = pkgs.lib.types.listOf pkgs.lib.types.str; };
      leadRepos = pkgs.lib.mkOption { type = pkgs.lib.types.listOf pkgs.lib.types.str; };
    };
    config.burrow.autoland = {
      pipelineRepos = [ "O/Web" ];
      leadRepos = [ "o/Lead" ];
    };
  };
in
pkgs.testers.runNixOSTest {
  name = "proj";

  nodes.machine = {
    imports = [
      self.nixosModules.proj
      self.nixosModules.watchdog
      pipelineRepos
    ];
    burrow = {
      claude.package = claude;
      # On, so proj keeps its waits and kicks it; it never runs here (no
      # burrow-events), the test reads what proj writes.
      watchdog.enable = true;
      proj = {
        enable = true;
        # Two slots for anyone, one more for pipeline jobs alone, and no cap
        # that binds before the slots run out (pick.sh's own check has those).
        maxJobs = 3;
        pipelineSlots = 1;
        maxJobsPerProject = 2;
        # The VM is small: only the slot count holds jobs back here.
        minFreeMemory = 0;
        jobMemoryMax = "128M";
        # Well past what the --first subtest takes from queueing its plain
        # job to freeing a slot for it (two runs, a pause and a stop, each
        # telling the watchdog; over 20s on a loaded host), so that job
        # never goes ahead for having waited.
        maxWait = 45;
        inherit gh;
        inbox = {
          enable = true;
          authors = [ "sam" "sam-steward[bot]" ];
        };
      };
      # The App the machine's own work acts as. Its socket isn't asked here:
      # what's checked is that a job runs in its env.
      github = {
        apps.workers = {
          id = 7;
          slug = "sam-workers";
          keyFile = "/run/keys/workers.pem";
          users = [ "dev" ];
        };
        machinery = "workers";
      };
    };
    # proj-watch and proj-inbox run by hand here, never from burrow-events,
    # whose kick port the test listens on itself.
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    environment.systemPackages = [
      pkgs.git
      pkgs.python3
    ];

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /data/dev/src/web 0755 dev dev -"
      "d /data/dev/src/lead 0755 dev dev -"
      "d /data/dev/src/ib 0755 dev dev -"
      "d /data/dev/src/bt 0755 dev dev -"
      "d /data/dev/src/uk 0755 dev dev -"
    ];
    virtualisation.memorySize = 2048;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import json
    import re
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def unit(job):
        return "proj-" + machine.succeed(f"systemd-escape {job}").strip() + ".service"

    def started(job):
        d = f"/data/dev/jobs/{job}"
        return float(machine.succeed(f"stat -c %.9Y {d}/$(cat {d}/turns).slot"))

    machine.wait_for_unit("user@1001.service")
    # burrow-events' kick port: each kick's path, a line each.
    machine.succeed("""cat >/tmp/kicks.py <<'EOF'
    import http.server
    class K(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length") or 0))
            open("/tmp/kicks", "a").write(self.path + "\\n")
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
    http.server.HTTPServer(("127.0.0.1", 8788), K).serve_forever()
    EOF""")
    machine.succeed("touch /tmp/kicks && (python3 /tmp/kicks.py >/tmp/kicks.log 2>&1 &)")
    machine.wait_for_open_port(8788)

    def kicks():
        # The watchdog's are looked at apart: proj kicks it at every change.
        return [k for k in machine.succeed("cat /tmp/kicks").split() if k != "/kick/watchdog"]

    def waits(until=None, timeout=30):
        # What proj last told the watchdog; with `until`, once that holds of
        # it (a turn's end is told after its state reads as ended).
        deadline = time.monotonic() + timeout
        while True:
            w = json.loads(machine.succeed("cat /data/dev/jobs/.waits.json 2>/dev/null || echo '{\"items\": {}}'"))["items"]
            if until is None or until(w):
                return w
            assert time.monotonic() < deadline, w
            time.sleep(0.5)

    def end(job, n=1):
        return machine.succeed(f"cat /data/dev/jobs/{job}/{n}.end").strip()

    with subtest("the cap is on claude.slice, none of it in swap"):
        out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax -p MemorySwapMax")
        assert "MemoryMax=" in out and "infinity" not in out and "MemorySwapMax=0" in out.splitlines(), out

    with subtest("a look with nothing awaited succeeds, and books nothing"):
        proj("_watch")
        assert json.loads(machine.succeed("cat /var/lib/burrow/proj-watch/wake.json")) == {}

    with subtest("a look waits for one already running"):
        # burrow-events runs one at a time, but one from before its restart
        # (or a hand-run one) can still be going: two would each write the
        # wake file from what they saw.
        machine.execute("su - dev -c 'flock /var/lib/burrow/proj-watch/wake.json.lock sleep 10' >/dev/null 2>&1 &")
        machine.wait_until_fails("flock -n /var/lib/burrow/proj-watch/wake.json.lock true", timeout=30)
        machine.fail("su - dev -c 'timeout 3 proj _watch'")
        machine.wait_until_succeeds("flock -n /var/lib/burrow/proj-watch/wake.json.lock true", timeout=30)

    with subtest("a turn that failed without saying anything has no answer"):
        e = proj("run web fail")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x error", timeout=120)
        out = machine.fail(f"su - dev -c 'proj log {e}'")
        assert "ended without an answer" in out and "refused" in out, out

    with subtest("a dot-named project's job isn't hidden from what goes through the jobs"):
        for d in (".dotted", "..dotted", ".-dotted"):
            machine.succeed(f"install -d -o dev -g dev /data/dev/src/{d}")
            j = proj(f'run {d} "sleep 1"')
            assert j[0] not in ".-", j
            assert j in proj("status"), proj("status")
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
            assert machine.succeed(f"cat /data/dev/jobs/{j}/project").strip() == f"/data/dev/src/{d}"
            machine.succeed(f"rm -rf /data/dev/src/{d}")

    with subtest("past maxJobs, jobs wait in line, and --first goes ahead"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run web "sleep 1"')
        d = proj('run --first web "sleep 1"')
        time.sleep(5)
        states = [state(j) for j in (a, b, c, d)]
        assert states == ["running", "running", "queued", "queued"], states
        listed = proj("status")
        assert ".queue" not in listed and ".slots" not in listed, listed
        out = machine.succeed(f"systemctl --user -M dev@ show '{unit(a)}' -p Slice -p MemoryMax")
        assert "Slice=claude-jobs.slice" in out and "MemoryMax=134217728" in out, out
        # One slot at a time: two free at once go to both in one look, and
        # which starts first is then only which looks first.
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        proj(f"stop {b}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        assert started(d) < started(c), (started(d), started(c))

    with subtest("proj withdraw ends a job only while its turn waits for a slot"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run web "sleep 1"')
        d = proj('run web "sleep 1"')
        time.sleep(5)
        assert [state(j) for j in (c, d)] == ["queued", "queued"], [state(j) for j in (c, d)]
        assert proj(f"withdraw {a}") == "running" and state(a) == "running", state(a)
        assert proj(f"withdraw {c}") == "withdrawn"
        assert state(c) not in ("queued", "running") and c not in waits(), (state(c), waits())
        assert proj(f"withdraw {c}") == "over"
        # d next in line, its job's lock held as withdraw would hold it: a's
        # slot comes free, and d waits for the lock to say it has it.
        machine.succeed(f"su - dev -c 'touch /data/dev/jobs/{d}/lock'")
        machine.execute(f"su - dev -c 'flock /data/dev/jobs/{d}/lock sleep 30' >/dev/null 2>&1 &")
        machine.wait_until_fails(f"flock -n /data/dev/jobs/{d}/lock true", timeout=30)
        proj(f"stop {a}")
        time.sleep(8)
        assert state(d) == "queued", state(d)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        proj(f"stop {b}")

    with subtest("a job whose turn has ended frees its slot"):
        a = proj('run web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        b = proj('run web "sleep 600"')
        c = proj('run web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        proj(f"stop {b}")

    with subtest("a turn stopped with work in the background hears so once, in the same slot, and answers again"):
        a = proj('run web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        assert proj(f"log {a}") == "waiting on CI\n\nthe build passed", proj(f"log {a}")
        assert machine.succeed(f"cat /data/dev/jobs/{a}/turns").strip() == "1"
        assert "--resume s" in machine.succeed("cat /tmp/cut-args")
        # Still up after being told: stopped for good, not told again.
        machine.succeed("echo linger >/tmp/cut-mode")
        b = proj('run web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x done", timeout=120)
        assert proj(f"log {b}") == "waiting on CI\n\nstill waiting", proj(f"log {b}")
        machine.succeed("rm /tmp/cut-mode")

    with subtest("what the first run left, in its own session and deaf to TERM, is gone before the second starts"):
        a = proj('run web "detach 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        assert proj(f"log {a}") == "waiting on CI\n\nthe build passed", proj(f"log {a}")
        assert machine.succeed("cat /tmp/strays").strip() == "0", machine.succeed("cat /tmp/strays")
        stream = machine.succeed(f"cat /data/dev/jobs/{a}/1.jsonl")
        assert "stray" in stream.split('"told"')[0], stream
        assert "stray" not in stream.split('"told"')[1], stream
        machine.fail("pgrep -f '^proj-stray'")

    with subtest("a second run that ends without an answer: the turn ends as it did, with the first answer kept"):
        for mode, st, note in [
            ("crash", "failed", "\n\n(turn 1 then ended: failed)"),
            ("signal", "failed", "\n\n(turn 1 then ended: failed)"),
            ("error", "error", '\n\n(turn 1 ended in an error)\n["boom"]'),
        ]:
            machine.succeed(f"echo {mode} >/tmp/cut-mode")
            a = proj('run web "linger 600"')
            machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x {st}", timeout=120)
            assert proj(f"log {a}") == "waiting on CI" + note, (mode, proj(f"log {a}"))
            assert machine.succeed(f"cat /data/dev/jobs/{a}/1.result") == "waiting on CI\n", mode
        # The turn's timeout is both runs': the second gets what's left.
        machine.succeed("echo hang >/tmp/cut-mode")
        t0 = time.monotonic()
        a = proj('run --timeout 15s web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'timed out after 15s'", timeout=120)
        assert time.monotonic() - t0 < 22, time.monotonic() - t0
        assert proj(f"log {a}") == "waiting on CI\n\n(turn 1 then ended: timed out after 15s)", proj(f"log {a}")
        # Its --idle counts from its own start.
        a = proj('run --idle 12s web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x 'went quiet for 12s'", timeout=120)
        assert proj(f"log {a}") == "waiting on CI\n\n(turn 1 then ended: went quiet for 12s)", proj(f"log {a}")
        assert machine.succeed("cat /tmp/strays").strip() == "0"
        machine.succeed("rm /tmp/cut-mode")

    with subtest("a second run whose unit goes under it: the log says the turn failed, then and after later turns"):
        machine.succeed("echo hang >/tmp/cut-mode")
        # Stopped, or its _exec killed before it could write how it ended.
        for how in ["stop", "kill --kill-whom=main --signal=KILL"]:
            machine.succeed("rm -f /tmp/cut-args")
            a = proj('run web "linger 600"')
            machine.wait_until_succeeds("test -e /tmp/cut-args", timeout=120)
            # Still going: nothing to say about how it ended.
            assert state(a) == "running", (how, state(a))
            assert proj(f"log {a}") == "waiting on CI", (how, proj(f"log {a}"))
            machine.succeed(f"systemctl --user -M dev@ {how} '{unit(a)}'")
            machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x failed", timeout=60)
            assert proj(f"log {a}") == "waiting on CI\n\n(turn 1 then ended: failed)", (how, proj(f"log {a}"))
            proj(f"reply {a} hello")
            machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
            log = proj(f"log {a} --turn 1")
            assert log == "waiting on CI\n\n(turn 1 then ended: failed)", (how, log)
        machine.succeed("rm /tmp/cut-mode")

    with subtest("a log waits out a turn being started, and tells only the turn it was asked for"):
        # A turn held halfway through starting (its number written, its unit
        # not yet), then put back: the log reads neither half alone.
        a = proj("run web hello")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        d = f"/data/dev/jobs/{a}"
        machine.execute(f"su - dev -c 'cd {d} && flock lock sh -c \"echo 2 >turns; sleep 10; echo 1 >turns\"' >/dev/null 2>&1 &")
        machine.wait_until_succeeds(f"grep -x 2 {d}/turns", timeout=30)
        machine.fail(f"su - dev -c 'timeout 3 proj log {a} --turn 1'")
        machine.wait_until_succeeds(f"grep -x 1 {d}/turns", timeout=30)
        assert proj(f"log {a} --turn 1") == "did hello", proj(f"log {a} --turn 1")

    with subtest("a turn whose time runs out while the first run's leftovers are stopped gets no second run"):
        # The stray ignores TERM, so stopping it takes about 10s, past the
        # 15s the turn had: no second run starts, not even a short one.
        machine.succeed("rm -f /tmp/cut-args")
        a = proj('run --timeout 15s web "detach 600"')
        machine.wait_until_fails(f"su - dev -c 'proj state {a}' | grep -qx -e queued -e running", timeout=120)
        assert state(a) == "timed out after 15s", state(a)
        machine.fail("test -e /tmp/cut-args")
        assert proj(f"log {a}") == "waiting on CI\n\n(turn 1 then ended: timed out after 15s)", proj(f"log {a}")
        machine.fail("pgrep -f '^proj-stray'")

    with subtest("a turn that doesn't say what comes next hears so once, and still silent is the watchdog's at once"):
        machine.succeed("touch /tmp/silent && truncate -s 0 /tmp/kicks && rm -f /tmp/told")
        a = proj("run --link https://github.com/o/r/issues/11 web silent")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        told = machine.succeed("cat /tmp/told")
        assert told.startswith("Your answer didn't say what comes next"), told
        assert all(x in told for x in ("proj await URL", "proj ask", "proj done")), told
        assert "proj done" in machine.succeed("cat /tmp/args")
        assert machine.succeed(f"cat /data/dev/jobs/{a}/turns").strip() == "1" and end(a) == "undeclared"
        assert proj(f"log {a}") == ("did silent\n\ntold\n\n(turn 1 ended without saying what comes next, "
                                    "even when asked again: nothing resumes the job but a reply)"), proj(f"log {a}")
        assert "[no next step]" in proj("status"), proj("status")
        ended = int(machine.succeed(f"stat -c %Y /data/dev/jobs/{a}/1.outcome"))
        w = waits(lambda w: "ended" in w.get(a, {}).get("phase", ""))[a]
        assert w["since"] == w["due"] == ended and w["kick"] is None, w
        assert w["link"] == "https://github.com/o/r/issues/11" and "without saying what comes next" in w["phase"], w
        assert "/kick/watchdog" in machine.succeed("cat /tmp/kicks").split()
        # A reply is its next turn, which says it's done: no longer watched.
        machine.succeed("rm /tmp/silent")
        proj(f'reply {a} "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        waits(lambda w: a not in w)
        assert end(a, 2) == "done"
        # Told, it says, in the same turn and slot: nothing to watch.
        b = proj("run web silent")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x done", timeout=120)
        assert proj(f"log {b}") == "did silent\n\ntold", proj(f"log {b}")
        waits(lambda w: b not in w)
        assert end(b) == "done"

    with subtest("a turn that answers waiting on its own background work, and says nothing more, is a stall"):
        # PSIV's inbox lanes #211 and #212: each answered that it was waiting
        # on a run it had left in the background, and nothing ever woke it.
        machine.succeed("touch /tmp/silent && rm -f /tmp/told")
        a = proj('run web "linger 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        told = machine.succeed("cat /tmp/told")
        assert told.startswith("Your answer ended the turn while") and "didn't say what comes next" in told, told
        assert told.count("A command that can outlast one Bash call") == 1, told
        assert end(a) == "undeclared"
        waits(lambda w: "without saying what comes next" in w.get(a, {}).get("phase", ""))
        # Ended by hand, it's no one's wait.
        proj(f"stop {a}")
        assert a not in waits(), waits()
        machine.succeed("rm /tmp/silent")

    with subtest("a question, a finished job and an --owned job's end aren't watched; a failure is"):
        a = proj("run web ask")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        assert end(a) == "ask" and "[asks]" in proj("status"), proj("status")
        waits(lambda w: a not in w)
        # Said from inside a job only, and as nothing more than that.
        machine.fail("su - dev -c 'proj done'")
        machine.fail(f"su - dev -c 'PROJ_JOB={a} proj ask \"why\"'")
        # --owned: what it kicks acts on its end, so it needn't say, isn't
        # told to, and its end is nobody else's.
        machine.fail("su - dev -c 'proj run --owned web hello'")
        machine.succeed("touch /tmp/silent")
        o = proj("run --owned --kick autoland/o/w web silent")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {o}' | grep -x done", timeout=120)
        assert proj(f"log {o}") == "did silent", proj(f"log {o}")
        assert "proj done" not in machine.succeed("cat /tmp/args")
        machine.fail(f"test -e /data/dev/jobs/{o}/1.end")
        o2 = proj("run --owned --kick autoland/o/w web fail")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {o2}' | grep -x error", timeout=120)
        waits(lambda w: o not in w and o2 not in w)
        machine.succeed("rm /tmp/silent")
        # Anyone else's failure is a stall at once: nothing resumes it.
        f = proj("run web fail")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x error", timeout=120)
        waits(lambda w: w.get(f, {}).get("phase") == "turn 1 ended (error), and nothing resumes it")
        proj(f"stop {f}")
        assert f not in waits(), waits()

    with subtest("the watchdog hears of a turn in line and one running, each to its deadline"):
        machine.succeed("truncate -s 0 /tmp/kicks")
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run web "sleep 1"')
        w = waits(lambda w: all(w.get(j, {}).get("phase") == "turn 1 is running" for j in (a, b)))
        assert w[a]["since"] == int(started(a)) and w[a]["due"] == w[a]["since"] + 4 * 3600 + 600, w[a]
        queued = int(machine.succeed(f"stat -c %Y /data/dev/jobs/{c}/prompt.1"))
        assert w[c]["phase"] == "turn 1 is waiting in line for a slot" and w[c]["due"] == queued + 7200, w[c]
        assert "/kick/watchdog" in machine.succeed("cat /tmp/kicks").split()
        # A turn's end changes what it waits on, and kicks it too.
        machine.succeed("truncate -s 0 /tmp/kicks")
        proj(f"stop {a}")
        assert a not in waits() and "/kick/watchdog" in machine.succeed("cat /tmp/kicks").split()
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        proj(f"stop {b}")
        waits(lambda w: a not in w and b not in w and c not in w)

    with subtest("a reply while the last turn's unit is still ending waits for it, and its turn isn't written before"):
        # The turn's end (_ended) held up on the watchdog's lock, as another
        # job's look at that moment would.
        a = proj('run web "sleep 5"')
        waits(lambda w: w.get(a, {}).get("phase") == "turn 1 is running")
        machine.succeed("touch /tmp/hold")
        machine.succeed("""(su - dev -c "flock /data/dev/jobs/.waits.lock sh -c 'touch /tmp/held; while [ -e /tmp/hold ]; do sleep 0.2; done'" >/dev/null 2>&1 &)""")
        machine.wait_until_succeeds("test -e /tmp/held")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        assert machine.succeed(f"systemctl --user -M dev@ show -P LoadState '{unit(a)}'").strip() == "loaded"
        machine.succeed(f"rm -f /tmp/reply.rc; ((su - dev -c 'proj reply {a} hello' >/tmp/reply.out 2>&1; echo $? >/tmp/reply.rc) >/dev/null 2>&1 &)")
        time.sleep(3)
        # Still waiting, and turn 2 not written: the ending turn's _ended
        # would read it as the latest, and as failed.
        machine.fail("test -e /tmp/reply.rc")
        assert machine.succeed(f"cat /data/dev/jobs/{a}/turns").strip() == "1"
        machine.fail(f"test -e /data/dev/jobs/{a}/prompt.2")
        machine.succeed("rm /tmp/hold /tmp/held")
        machine.wait_until_succeeds("test -e /tmp/reply.rc", timeout=60)
        assert machine.succeed("cat /tmp/reply.rc").strip() == "0", machine.succeed("cat /tmp/reply.out")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        assert machine.succeed(f"cat /data/dev/jobs/{a}/turns").strip() == "2"
        waits(lambda w: a not in w)

    with subtest("a timeout reads as timeout(1) reads it: fractions, and 0 for none"):
        a = proj("run web hello")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
        for limit, task, st in [
            ("0", "sleep 3", "done"),
            ("1.5h", "hello", "done"),
            ("0.05m", "sleep 600", "timed out after 0.05m"),
            # A fraction of a second is kept, not rounded up to one.
            ("0.1s", "sleep 0.5", "timed out after 0.1s"),
            ("soon", "hello", "failed"),
        ]:
            machine.succeed(f"echo {limit} >/data/dev/jobs/{a}/timeout")
            proj(f'reply {a} "{task}"')
            machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x '{st}'", timeout=120)

    with subtest("a job run --app acts as that App every turn, or not at all"):
        e = proj("run --app workers web showenv")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        # The App's socket and git's settings, and no token of the user's.
        def acts_as_workers(job):
            out = proj(f"log {job}")
            assert re.fullmatch(r"did socket=/run/gh-token/workers\.sock count=[1-9][0-9]* token=", out), out
        acts_as_workers(e)
        assert "[as workers]" in proj("status"), proj("status")
        # A login's own settings and token come before the App's, so they
        # can't undo it.
        machine.succeed("echo 'export GH_TOKEN_SOCKET= GIT_CONFIG_COUNT=0 GH_TOKEN=mine' >/home/dev/.bash_profile")
        proj(f"reply {e} showenv")
        machine.wait_until_succeeds(f"grep -x done /data/dev/jobs/{e}/2.outcome", timeout=120)
        acts_as_workers(e)
        # Without --app, the user's own.
        u = proj("run web showenv")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {u}' | grep -x done", timeout=120)
        assert proj(f"log {u}") == "did socket= count=0 token=mine", proj(f"log {u}")
        machine.succeed("rm /home/dev/.bash_profile")
        # An App the machine hasn't, or one with a sandbox: refused.
        machine.fail("su - dev -c 'proj run --app nosuch web showenv'")
        machine.fail("su - dev -c 'proj run --app ../workers web showenv'")
        machine.fail("su - dev -c 'proj run --vm --app workers web showenv'")
        machine.fail(f"su - dev -c 'proj reply --app workers {e} showenv'")
        # One whose App has gone fails its turn rather than run as the user.
        machine.succeed(f"echo gone >/data/dev/jobs/{e}/app")
        proj(f"reply {e} showenv")
        machine.wait_until_succeeds(f"grep -q 'no GitHub App gone' /data/dev/jobs/{e}/3.outcome", timeout=120)
        assert not machine.succeed(f"cat /data/dev/jobs/{e}/3.jsonl 2>/dev/null || true").strip()
        # One whose App goes while it waits for a slot starts as the App all
        # the same, never as the user: its env was read before it waited.
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        q = proj("run --app workers web showenv")
        machine.wait_until_succeeds(f"ls /data/dev/jobs/.queue | grep -q -- '-{q}$'", timeout=120)
        machine.succeed("mv /etc/gh-token/workers.env /tmp/workers.env")
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x done", timeout=120)
        machine.succeed("mv /tmp/workers.env /etc/gh-token/workers.env")
        proj(f"stop {b}")
        acts_as_workers(q)

    with subtest("a turn that has waited past maxWait goes ahead of a newer --first"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run web "sleep 1"')
        time.sleep(50)
        d = proj('run --first web "sleep 1"')
        time.sleep(5)
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        proj(f"stop {b}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        assert started(c) < started(d), (started(c), started(d))

    with subtest("a pipeline job in the pipeline's own repo takes its own slot while the others are full, every turn of it"):
        machine.succeed("su - dev -c 'git -C /data/dev/src/web init -q && git -C /data/dev/src/web remote add origin https://github.com/o/web.git'")
        # A GitHub clone now, so proj-inbox looks at o/web too: it has no issues.
        machine.succeed("mkdir -p /tmp/gh && echo '[]' >/tmp/gh/repos_o_web_issues")
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run --first web "sleep 1"')
        q = proj('run web "sleep 1"')
        # The whole line, next first: --first ahead of plain.
        # A run returns before its turn has its place: wait for both.
        machine.wait_until_succeeds(f"ls /data/dev/jobs/.queue | grep -c -e '-{c}$' -e '-{q}$' | grep -x 2", timeout=120)
        line = proj("status --line").splitlines()
        assert [(l.split()[0], l.split()[1]) for l in line] == [(c, "first"), (q, "normal")], line
        proj(f"stop {q}")
        p = proj('run --pipeline web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {p}' | grep -x done", timeout=120)
        proj(f'reply {p} "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {p}' | grep -x done", timeout=120)
        assert state(c) == "queued", state(c)
        assert "asked as --pipeline" not in proj("status"), proj("status")
        machine.fail(f"""su - dev -c 'proj reply --pipeline {p} "sleep 1"'""")
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        proj(f"stop {b}")

    with subtest("a turn kicks what it was started to once it takes its slot, not while it waits"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        machine.succeed("truncate -s 0 /tmp/kicks")
        q = proj('run --kick autoland/o/s web "sleep 600"')
        time.sleep(5)
        assert state(q) == "queued" and kicks() == [], (state(q), kicks())
        proj(f"stop {a}")
        machine.wait_until_succeeds("grep -x /kick/autoland/o/s /tmp/kicks", timeout=60)
        assert state(q) == "running" and kicks() == ["/kick/autoland/o/s"], (state(q), kicks())
        proj(f"stop {b}")
        proj(f"stop {q}")

    with subtest("a pipeline job in a lead repo goes ahead of one in another pipeline repo queued before it"):
        machine.succeed("su - dev -c 'git -C /data/dev/src/lead init -q && git -C /data/dev/src/lead remote add origin https://github.com/o/lead.git'")
        machine.succeed("echo '[]' >/tmp/gh/repos_o_lead_issues")
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        q = proj('run --pipeline web "sleep 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x running", timeout=120)
        p = proj('run --pipeline web "sleep 1"')
        time.sleep(3)
        lead = proj('run --pipeline lead "sleep 1"')
        time.sleep(3)
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {p}' | grep -x done", timeout=120)
        assert started(lead) < started(p), (started(lead), started(p))
        assert "asked as --pipeline" not in proj("status"), proj("status")
        proj(f"stop {b}")
        proj(f"stop {q}")

    with subtest("a lead job leaves the pipeline's own slot to another pipeline job in line, and takes it when none is"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        q = proj('run --pipeline web "sleep 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x running", timeout=120)
        lead = proj('run --pipeline lead "sleep 600"')
        time.sleep(3)
        p = proj('run --pipeline web "sleep 1"')
        time.sleep(3)
        proj(f"stop {q}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {p}' | grep -x done", timeout=120)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {lead}' | grep -x running", timeout=120)
        assert started(p) < started(lead), (started(p), started(lead))
        proj(f"stop {lead}")
        lead = proj('run --pipeline lead "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {lead}' | grep -x done", timeout=120)
        proj(f"stop {a}")
        proj(f"stop {b}")

    with subtest("an --unstick job takes its own slot while every other is held and others wait, every turn of it, one at a time"):
        x = proj('run web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {x}' | grep -x done", timeout=120)
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        q = proj('run --pipeline web "sleep 600"')
        for j in (a, b, q):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run --first web "sleep 1"')
        u = proj('run --unstick web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {u}' | grep -x done", timeout=120)
        proj(f'reply {u} "sleep 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {u}' | grep -x running", timeout=120)
        machine.fail(f"""su - dev -c 'proj reply --unstick {u} "sleep 1"'""")
        v = proj('run --unstick web "sleep 1"')
        w = waits(lambda w: w.get(v, {}).get("unstick") is True)
        assert w[v]["line"] and w[v]["phase"] == "turn 1 is waiting for the --unstick slot, which another --unstick turn holds", w[v]
        assert w[c]["unstick"] is False and w[u]["unstick"] is True, (w[c], w[u])
        # Made one later (proj unstick): a turn waiting in line leaves it, and
        # a job that has run takes that slot from its next turn.
        p = proj('run web "sleep 1"')
        time.sleep(3)
        assert state(p) == "queued", state(p)
        proj(f"unstick {p}")
        proj(f"unstick {x}")
        proj(f'reply {x} "sleep 1"')
        waits(lambda w: all(w.get(j, {}).get("unstick") is True for j in (p, x)))
        time.sleep(3)
        assert [state(j) for j in (c, v, p, x)] == ["queued"] * 4, [state(j) for j in (c, v, p, x)]
        proj(f"stop {u}")
        for j in (v, p, x):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
        assert state(c) == "queued", state(c)
        for j in (a, b, q):
            proj(f"stop {j}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)

    with subtest("a pipeline job anywhere else goes as --first, and says so in proj status and its turn's journal"):
        q = proj('run --pipeline ib "sleep 1"')
        demoted = "asked as --pipeline, but its project has no origin: it goes as --first"
        listed = proj("status")
        assert f"[{demoted}]" in listed, listed
        # A line a job, whatever columns it has: each starts with its id.
        jobs = machine.succeed("ls -1 /data/dev/jobs").splitlines()
        assert all(any(line.startswith(j + " ") for j in jobs) for line in listed.splitlines()), listed
        machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x done", timeout=120)
        machine.wait_until_succeeds(f"journalctl -q '_SYSTEMD_USER_UNIT={unit(q)}' | grep -qF \"proj: {demoted}\"", timeout=30)
  ''
  # proj await, proj-watch and proj-inbox, on this machine and its stand-ins.
  + import ./proj-github.nix { inherit pkgs; }
  + ''
    with subtest("an earlier turn's answer is read without the turn count"):
        e = proj('run web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        machine.succeed(f"mv /data/dev/jobs/{e}/turns /tmp/turns-aside")
        assert proj(f"log {e} --turn 1") == "did sleep 1"
        machine.succeed(f"mv /tmp/turns-aside /data/dev/jobs/{e}/turns")

    with subtest("a job with no turn count doesn't stop _watch looking at the rest"):
        machine.succeed("su - dev -c 'mkdir -p /data/dev/jobs/broken && printf \"o/r#6\\t0\\t{}\\n\" >/data/dev/jobs/broken/await'")
        proj("_watch")
        machine.succeed("rm -rf /data/dev/jobs/broken")

    with subtest("every answer in a turn is kept, not only the last"):
        f = proj("run web twice")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x done", timeout=120)
        assert proj(f"log {f}") == "the answer\n\ndid twice", proj(f"log {f}")
        assert machine.succeed(f"cat /data/dev/jobs/{f}/1.result") == "the answer\n\ndid twice\n"

    with subtest("an answer, then an error: the log has both"):
        f = proj("run web answerfail")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x error", timeout=120)
        log = proj(f"log {f}")
        assert log == 'the answer\n\n(turn 1 ended in an error)\n["refused"]', log

    with subtest("a job past its memory cap is killed by its own limit"):
        e = proj("run web eat")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x failed", timeout=120)
        machine.succeed("journalctl -k | grep -q 'Memory cgroup out of memory'")

    with subtest("a turn killed before it answers can still be continued, in its own session"):
        # As its unit goes when systemd-oomd kills it, or when Claude itself is
        # what outgrows the job's memory cap.
        e = proj('run web "named 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x running", timeout=120)
        machine.wait_until_succeeds(f"grep -q named /data/dev/jobs/{e}/1.jsonl", timeout=60)
        machine.succeed(f"systemctl --user -M dev@ kill --signal=KILL '{unit(e)}'")
        machine.wait_until_fails(f"systemctl --user -M dev@ is-active -q '{unit(e)}'", timeout=60)
        proj(f'reply {e} "carry on"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        assert "--resume named" in machine.succeed("cat /tmp/args"), machine.succeed("cat /tmp/args")

    with subtest("a command a job runs past its cap is killed, and the turn carries on to its answer"):
        o = proj("run web outgrow")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {o}' | grep -x done", timeout=120)
        answer = proj(f"log {o}")
        assert "it exited 137" in answer, answer
  '';
}
