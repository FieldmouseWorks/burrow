# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead but can't starve a turn that has waited long, that a job
# whose turn has ended frees its slot, that one stopped after its answer with
# work in the background has the rest of that run stopped, hears so once and
# answers again (the turn ending as its last run did, its log saying so even
# when its unit went first, and within one timeout that the clearing up
# between runs counts against),
# that one waiting on a PR or issue
# (proj await) is resumed when it changes, even before the turn ended and
# from proj-watch's own unit, but never for proj-inbox's own comment, or once
# its deadline passes with GitHub unreadable, that a reply meanwhile owns the
# job's waits, that a turn's end kicks what it was started to and proj-watch
# for the repos it awaits, that a job's own --timeout stops its turns, and
# its --idle one that has gone quiet (and either still kicks), that _watch and proj-inbox given a repo look at
# that repo alone and book when it next needs a look, that proj-inbox posts
# every finished turn's answer (one
# resumed before it looked too, none misread while a turn starts) and ends
# the waits of one whose issue closed (proj stop's, which a watch already
# asking GitHub keeps to), a stop that fails there not holding up the rest,
# that a job dir removed or a wait it can't read stops neither proj-inbox nor
# _watch, and that a job past its memory cap is killed by its own cgroup's
# limit rather than the machine running out.
#
#   nix build .#checks.x86_64-linux.proj -L
{ self, pkgs }:
let
  # Claude, as far as proj can tell: reads the prompt, answers in stream-json.
  # "sleep N" sleeps; "eat" holds far more memory than the test's job cap;
  # "twice" answers twice in one turn, "answerfail" answers and then fails;
  # "linger N" answers, then stays up N seconds, as Claude does while
  # something it left in the background (a CI watch, a long build) is still
  # going; "detach N" does too, having left work in its own session that
  # ignores TERM and writes to the stream. Told so, it answers again, or as
  # /tmp/cut-mode says: "linger" answers and stays up, "hang" says nothing,
  # "crash" and "signal" die without a word, "error" fails;
  # "chatter N" says something every second for N seconds, then answers.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    case $prompt in
      sleep*) sleep "''${prompt#sleep }" ;;
      chatter*) for _ in $(seq "''${prompt#chatter }"); do echo '{"type":"system"}'; sleep 1; done ;;
      linger*)
        echo '{"type":"result","session_id":"s","result":"waiting on CI","is_error":false}'
        sleep "''${prompt#linger }"
        exit 0 ;;
      detach*)
        # shellcheck disable=SC2016
        setsid bash -c 'trap "" TERM; exec -a proj-stray bash -c "while :; do echo stray; sleep 1; done"' &
        echo '{"type":"result","session_id":"s","result":"waiting on CI","is_error":false}'
        sleep "''${prompt#detach }"
        exit 0 ;;
      "Your answer ended the turn while"*)
        echo "$*" >/tmp/cut-args
        { pgrep -cf '^proj-stray' || true; } >/tmp/strays
        echo '{"type":"system","subtype":"told"}'
        case $(cat /tmp/cut-mode 2>/dev/null) in
          linger)
            echo '{"type":"result","session_id":"s","result":"still waiting","is_error":false}'
            sleep 600 ;;
          hang) sleep 600 ;;
          crash) exit 3 ;;
          signal) kill -KILL $$ ;;
          error)
            echo '{"type":"result","session_id":"s","is_error":true,"errors":["boom"]}'
            exit 1 ;;
          *) echo '{"type":"result","session_id":"s","result":"the build passed","is_error":false}' ;;
        esac
        exit 0 ;;
      await*) proj await "''${prompt#await }" >/dev/null ;;
      # Awaits, then takes a while to answer: what changes meanwhile is news.
      slowawait*) proj await "''${prompt#slowawait }" >/dev/null; sleep 10 ;;
      eat) head -c 512M /dev/zero | tail >/dev/null; exit 1 ;;
      # Answers, then answers again once a background task reports back.
      twice) echo '{"type":"result","session_id":"s","result":"the answer","is_error":false}' ;;
      # Answers, then fails as "fail" does.
      answerfail)
        echo '{"type":"result","session_id":"s","result":"the answer","is_error":false}'
        echo '{"type":"result","session_id":"s","is_error":true,"errors":["refused"]}'
        exit 1 ;;
      # Fails as Claude does when the API refuses it: a result with no text.
      fail) echo '{"type":"result","session_id":"s","is_error":true,"errors":["refused"]}'; exit 1 ;;
      # An issue's task (proj-inbox) that takes a while to answer.
      *inbox-slow*) sleep 8 ;;
    esac
    ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'
  '';
  # GitHub, as far as proj-watch and proj-inbox can tell: each endpoint's
  # answer is a file in /tmp/gh that the test writes. A file holding one JSON
  # value per line is one page per line, which --slurp gathers into an array
  # as gh does. What's posted (-F body=@-) is added to /tmp/gh/posted.
  gh = pkgs.writeShellScriptBin "gh" ''
    [ "$1" = api ] || exit 2
    shift
    slurp="" post=""
    while [ $# -gt 0 ]; do
      case $1 in
        --slurp) slurp=1 ;;
        -F | -f) post=1; shift ;;
        -*) ;;
        *) ep=''${1%%\?*} ;;
      esac
      shift
    done
    if [ -n "$post" ]; then cat >>/tmp/gh/posted; echo '{}'; exit 0; fi
    # /tmp/gh/slow: GitHub takes its time, as it does.
    [ -e /tmp/gh/slow ] && sleep 2
    f="/tmp/gh/''${ep//\//_}"
    [ -e "$f" ] || exit 1
    if [ -n "$slurp" ]; then ${pkgs.jq}/bin/jq -cs . "$f"; else cat "$f"; fi
  '';
in
pkgs.testers.runNixOSTest {
  name = "proj";

  nodes.machine = {
    imports = [ self.nixosModules.proj ];
    burrow = {
      claude.package = claude;
      proj = {
        enable = true;
        maxJobs = 2;
        # The VM is small: only the slot count holds jobs back here.
        minFreeMemory = 0;
        jobMemoryMax = "128M";
        maxWait = 20;
        inherit gh;
        inbox = {
          enable = true;
          authors = [ "sam" ];
        };
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
      "d /data/dev/src/ib 0755 dev dev -"
    ];
    virtualisation.memorySize = 2048;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import json
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
        return machine.succeed("cat /tmp/kicks").split()

    with subtest("the cap is on claude.slice"):
        out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax")
        assert "MemoryMax=" in out and "infinity" not in out, out

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
        proj(f"stop {a}")
        proj(f"stop {b}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        assert started(d) < started(c), (started(d), started(c))

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

    with subtest("a turn that has waited past maxWait goes ahead of a newer --first"):
        a = proj('run web "sleep 600"')
        b = proj('run web "sleep 600"')
        for j in (a, b):
            machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x running", timeout=120)
        c = proj('run web "sleep 1"')
        time.sleep(25)
        d = proj('run --first web "sleep 1"')
        time.sleep(5)
        proj(f"stop {a}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        proj(f"stop {b}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        assert started(c) < started(d), (started(c), started(d))

    with subtest("a job waiting on a PR is resumed when the PR changes"):
        def github(endpoint, answer):
            machine.succeed(f"mkdir -p /tmp/gh && echo '{answer}' >/tmp/gh/{endpoint.replace('/', '_')}")
        github("repos/o/r/issues/5", '{"pull_request": {}, "state": "open", "comments": 0}')
        github("repos/o/r/pulls/5", '{"state": "open", "merged": false, "head": {"sha": "abc"}, "comments": 0, "review_comments": 0}')
        github("repos/o/r/pulls/5/reviews", '[]')
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "in_progress", "conclusion": null}]}')
        github("repos/o/r/commits/abc/status", '{"state": "pending", "total_count": 0}')
        github("repos/o/r/issues/5/comments", '[]')
        machine.succeed("truncate -s 0 /tmp/kicks")
        e = proj('run --kick autoland/o/r web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        assert "[awaits o/r#5]" in proj("status"), proj("status")
        # Its turn's end kicked what it was started to, and proj-watch for o/r.
        machine.wait_until_succeeds("grep -x /kick/proj-watch/o/r /tmp/kicks", timeout=30)
        assert sorted(kicks()) == ["/kick/autoland/o/r", "/kick/proj-watch/o/r"], kicks()
        # A project folder's name is the job's, whatever it holds.
        machine.succeed("install -d -o dev -g dev '/data/dev/src/my web%n'")
        machine.succeed("truncate -s 0 /tmp/kicks")
        e2 = proj('run --kick autoland/o/q \"my web%n\" hello')
        machine.wait_until_succeeds(f"su - dev -c 'proj state \"{e2}\"' | grep -x done", timeout=120)
        machine.wait_until_succeeds("grep -x /kick/autoland/o/q /tmp/kicks", timeout=30)
        # A job's own --timeout stops its turn then, well before TIMEOUT, and
        # the stop still kicks what it was started to.
        machine.succeed("truncate -s 0 /tmp/kicks")
        e3 = proj('run --timeout 3s --kick autoland/o/t web "sleep 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e3}' | grep -x 'timed out after 3s'", timeout=60)
        machine.wait_until_succeeds("grep -x /kick/autoland/o/t /tmp/kicks", timeout=30)
        machine.fail("su - dev -c 'proj run --timeout soon web hello'")
        # One whose stream says nothing for its --idle is stuck, and stopped
        # then, kicking as before; one that keeps talking runs on past it.
        e4 = proj('run --idle 3s --kick autoland/o/i web "sleep 600"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e4}' | grep -x 'went quiet for 3s'", timeout=60)
        machine.wait_until_succeeds("grep -x /kick/autoland/o/i /tmp/kicks", timeout=30)
        e5 = proj('run --idle 3s web "chatter 8"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e5}' | grep -x done", timeout=60)
        machine.fail("su - dev -c 'proj run --idle 0s web hello'")
        # proj-watch's next look at o/r is booked for the wait's deadline.
        since = int(machine.succeed(f"cut -f2 /data/dev/jobs/{e}/await").strip())
        proj("_watch")
        wake = json.loads(machine.succeed("cat /var/lib/burrow/proj-watch/wake.json"))
        assert wake == {"o/r": since + 24 * 3600 + 1}, wake
        # proj-inbox's own answer on the PR isn't news.
        github("repos/o/r/pulls/5", '{"state": "open", "merged": false, "head": {"sha": "abc"}, "comments": 1, "review_comments": 0}')
        github("repos/o/r/issues/5/comments", '[{"id": 1, "body": "did it\\n<!-- proj-inbox -->"}]')
        proj("_watch")
        time.sleep(3)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}')
        # Another repo's look leaves it be.
        proj("_watch o/q")
        time.sleep(3)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        # burrow-events' way: the repo's own unit, running as the user, and
        # spelt as GitHub may spell it.
        machine.succeed("systemctl start proj-watch@O-R.service")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "o/r#5: checks" in prompt and "ci=success" in prompt, prompt
        assert "awaits" not in proj("status"), proj("status")

    with subtest("a review past the first page is news"):
        # A hundred reviews with bodies, past what one argument may hold all
        # together (and what one command to the VM may, so it's made there).
        machine.succeed("${pkgs.jq}/bin/jq -nc '[range(100) | {id: ., state: \"COMMENTED\", body: (\"x\" * 2000)}]' >/tmp/gh/repos_o_r_pulls_5_reviews")
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}')
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        machine.succeed("echo '[{\"id\": 100, \"state\": \"COMMENTED\"}]' >>/tmp/gh/repos_o_r_pulls_5_reviews")
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert 'reviews {"COMMENTED":100} → {"COMMENTED":101}' in prompt, prompt
        github("repos/o/r/pulls/5/reviews", '[]')

    with subtest("a review dismissed is news"):
        github("repos/o/r/pulls/5/reviews", '[{"id": 1, "state": "CHANGES_REQUESTED"}]')
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        github("repos/o/r/pulls/5/reviews", '[{"id": 1, "state": "DISMISSED"}]')
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "DISMISSED" in prompt, prompt
        github("repos/o/r/pulls/5/reviews", '[]')

    with subtest("a resume that can't start keeps the job's waits"):
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "in_progress", "conclusion": null}]}')
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}')
        # Its deadline has passed too, so the retry is all that books o/r.
        machine.succeed(f"sed -i 's/\\t[0-9]*\\t/\\t0\\t/' /data/dev/jobs/{e}/await")
        # No user manager to reach (a bus error, a restart): the turn can't start.
        before = int(machine.succeed("date +%s"))
        machine.execute("su - dev -c 'XDG_RUNTIME_DIR=/nonexistent proj _watch'")
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        machine.succeed(f"test -s /data/dev/jobs/{e}/await")
        # Retried in five minutes: the past deadline doesn't take its place.
        wake = json.loads(machine.succeed("cat /var/lib/burrow/proj-watch/wake.json"))
        assert before + 300 <= wake["o/r"] <= int(machine.succeed("date +%s")) + 300, wake
        # A look at another repo keeps o/r's retry.
        machine.execute("su - dev -c 'XDG_RUNTIME_DIR=/nonexistent proj _watch o/q'")
        kept = json.loads(machine.succeed("cat /var/lib/burrow/proj-watch/wake.json"))
        assert kept.get("o/r") == wake["o/r"], kept
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)

    with subtest("a change after proj await but before the turn ends is news"):
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "in_progress", "conclusion": null}]}')
        e = proj('run web "slowawait https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"test -s /data/dev/jobs/{e}/await.next", timeout=120)
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "failure"}]}')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "ci=failure" in prompt, prompt

    with subtest("a reply while GitHub is being asked owns the job's waits"):
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "in_progress", "conclusion": null}]}')
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        machine.succeed("touch /tmp/gh/slow")
        machine.succeed("su - dev -c 'proj _watch' >/tmp/watch.log 2>&1 &")
        time.sleep(2)
        proj(f'reply {e} "sleep 1"')
        machine.wait_until_succeeds("! pgrep -u dev -f 'proj _watch'", timeout=60)
        machine.succeed("rm /tmp/gh/slow")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "2"
        machine.fail(f"test -e /data/dev/jobs/{e}/await")

    with subtest("the inbox's own answer on an awaited issue isn't news"):
        github("repos/o/r/issues/6", '{"state": "open", "comments": 0}')
        github("repos/o/r/issues/6/comments", '[]')
        e = proj('run web "await https://github.com/o/r/issues/6"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        github("repos/o/r/issues/6", '{"state": "open", "comments": 1}')
        github("repos/o/r/issues/6/comments", '[{"id": 1, "body": "did it\\n<!-- proj-inbox -->"}]')
        proj("_watch")
        time.sleep(3)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        github("repos/o/r/issues/6", '{"state": "open", "comments": 2}')
        github("repos/o/r/issues/6/comments", '[{"id": 1, "body": "did it\\n<!-- proj-inbox -->"}, {"id": 2, "body": "yes"}]')
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)

    with subtest("the await deadline holds while GitHub can't be read"):
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "in_progress", "conclusion": null}]}')
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        # Waiting since 1970, and GitHub answers nothing.
        machine.succeed(f"sed -i 's/\\t[0-9]*\\t/\\t0\\t/' /data/dev/jobs/{e}/await")
        machine.succeed("rm /tmp/gh/repos_o_r_issues_5")
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "couldn't be read from GitHub" in prompt, prompt

    with subtest("every finished turn's answer reaches its issue, one resumed before the inbox looked too"):
        # The stand-in gh runs as dev and appends what it posts here.
        machine.succeed("mkdir -p /tmp/gh && install -m 666 /dev/null /tmp/gh/posted")
        machine.succeed("su - dev -c 'git -C /data/dev/src/ib init -q && git -C /data/dev/src/ib remote add origin https://github.com/o/r'")
        issue = {"number": 7, "user": {"login": "sam"}, "html_url": "https://github.com/o/r/issues/7",
                 "title": "t", "body": "inbox-slow", "state": "open", "labels": [{"name": "claude"}]}
        github("repos/o/r/issues", json.dumps([issue]))
        github("repos/o/r/issues/7/comments", "[]")
        # Another repo's look leaves it be.
        machine.succeed("su - dev -c 'proj-inbox o/q'")
        assert machine.succeed("su - dev -c 'proj-inbox status'").strip() == ""
        machine.succeed("truncate -s 0 /tmp/kicks")
        # Spelt otherwise than the clone's origin: GitHub's names are case-blind.
        machine.succeed("su - dev -c 'proj-inbox O/R'")
        job = machine.succeed("su - dev -c 'proj-inbox status'").split()[-1]
        machine.wait_until_succeeds(f"su - dev -c 'proj state {job}' | grep -x done", timeout=120)
        # Its turn's end kicks the inbox's look at o/r, which posts the answer.
        machine.wait_until_succeeds("grep -x /kick/proj-inbox/o/r /tmp/kicks", timeout=30)
        # Its next turn starts before the inbox looks again, as proj _watch's can.
        proj(f'reply {job} "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {job}' | grep -x done", timeout=120)
        machine.succeed("su - dev -c proj-inbox")
        posted = machine.succeed("cat /tmp/gh/posted")
        first = posted.find("did This task comes from GitHub issue o/r#7")
        assert "On it" in posted and -1 < first < posted.find("did sleep 1"), posted

    with subtest("a reply under a minute old waits, and the inbox books when it's old enough"):
        now = machine.succeed("date -u +%Y-%m-%dT%H:%M:%SZ").strip()
        github("repos/o/r/issues/7", json.dumps(issue))
        github("repos/o/r/issues/7/comments", json.dumps([{"id": 50, "user": {"login": "sam"}, "body": "and the tests", "created_at": now}]))
        machine.succeed("su - dev -c 'proj-inbox o/r'")
        assert machine.succeed(f"cat /data/dev/jobs/{job}/turns").strip() == "2"
        wake = json.loads(machine.succeed("cat /var/lib/burrow/proj-inbox/wake.json"))
        at = int(machine.succeed(f"date -d {now} +%s").strip()) + 61
        assert wake == {"o/r": at}, (wake, at)
        github("repos/o/r/issues/7/comments", "[]")
        machine.succeed("su - dev -c 'proj-inbox o/r'")
        assert json.loads(machine.succeed("cat /var/lib/burrow/proj-inbox/wake.json")) == {}
        # Its own answer, or someone else's comment, is nothing to hand on.
        now = machine.succeed("date -u +%Y-%m-%dT%H:%M:%SZ").strip()
        github("repos/o/r/issues/7/comments", json.dumps([
            {"id": 51, "user": {"login": "sam"}, "body": "did it <!-- proj-inbox -->", "created_at": now},
            {"id": 52, "user": {"login": "eve"}, "body": "me too", "created_at": now}]))
        machine.succeed("su - dev -c 'proj-inbox o/r'")
        assert json.loads(machine.succeed("cat /var/lib/burrow/proj-inbox/wake.json")) == {}
        github("repos/o/r/issues/7/comments", "[]")
        machine.succeed("su - dev -c 'proj-inbox o/r'")

    with subtest("a reply whose turn can't start is tried again in five minutes"):
        old = machine.succeed("date -u -d '2 minutes ago' +%Y-%m-%dT%H:%M:%SZ").strip()
        github("repos/o/r/issues/7/comments", json.dumps([{"id": 60, "user": {"login": "sam"}, "body": "and the docs", "created_at": old}]))
        before = int(machine.succeed("date +%s"))
        machine.succeed("su - dev -c 'env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR=/nonexistent proj-inbox o/r'")
        assert machine.succeed(f"cat /data/dev/jobs/{job}/turns").strip() == "2"
        wake = json.loads(machine.succeed("cat /var/lib/burrow/proj-inbox/wake.json"))
        assert before + 300 <= wake.get("o/r", 0) <= int(machine.succeed("date +%s")) + 300, wake
        github("repos/o/r/issues/7/comments", "[]")
        machine.succeed("su - dev -c 'proj-inbox o/r'")

    with subtest("the inbox waits for a pass already running"):
        # Two at once could each find an issue without its job and start one.
        machine.execute("su - dev -c 'flock /var/lib/burrow/proj-inbox/wake.json.lock sleep 10' >/dev/null 2>&1 &")
        machine.wait_until_fails("flock -n /var/lib/burrow/proj-inbox/wake.json.lock true", timeout=30)
        machine.fail("su - dev -c 'timeout 3 proj-inbox o/r'")
        machine.wait_until_succeeds("flock -n /var/lib/burrow/proj-inbox/wake.json.lock true", timeout=30)

    with subtest("the inbox waits out a turn being started"):
        # A turn held halfway through starting (its number written, its unit
        # not yet), then put back: the inbox reads neither half alone.
        d = f"/data/dev/jobs/{job}"
        machine.execute(f"su - dev -c 'cd {d} && flock lock sh -c \"echo 3 >turns; sleep 20; echo 2 >turns\"' >/dev/null 2>&1 &")
        machine.wait_until_succeeds(f"grep -x 3 {d}/turns", timeout=30)
        machine.succeed("su - dev -c proj-inbox")
        posted = machine.succeed("cat /tmp/gh/posted")
        assert "turn 3" not in posted, posted
        machine.succeed(f"grep -x 2 {d}/turns")

    with subtest("a turn that never got going isn't read as the last one's outcome"):
        # Numbered, but its _exec gone before it wrote anything (killed as it
        # started): the turn failed, whatever the turn before it did.
        d = f"/data/dev/jobs/{job}"
        machine.succeed(f"su - dev -c 'echo 3 >{d}/turns'")
        assert state(job) == "failed", state(job)
        machine.succeed(f"su - dev -c 'echo 2 >{d}/turns'")
        assert state(job) == "done", state(job)

    with subtest("a job from before each turn kept its own files reads as it was"):
        # Its last turn's outcome and answer unnumbered, written after its
        # prompt (the update came between turns, or ran across one).
        machine.succeed(f"su - dev -c 'cd {d} && mv 2.outcome outcome && mv 2.result result && touch outcome result'")
        assert state(job) == "done", state(job)
        machine.succeed(f"test -s {d}/2.outcome && test -e {d}/2.result && ! test -e {d}/outcome && ! test -e {d}/result")
        # One older than the latest turn's prompt is an earlier turn's, not its.
        machine.succeed(f"su - dev -c 'cd {d} && mv 2.outcome outcome && touch -d \"1 hour ago\" outcome'")
        assert state(job) == "failed", state(job)
        machine.fail(f"test -e {d}/outcome")
        machine.succeed(f"su - dev -c 'echo done >{d}/2.outcome'")
        assert state(job) == "done", state(job)

    with subtest("a turn running across the update keeps its outcome until it ends"):
        # The old _exec reads its outcome back to publish its waits.
        e = proj('run web "sleep 30"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x running", timeout=60)
        machine.succeed(f"su - dev -c 'echo done >/data/dev/jobs/{e}/outcome'")
        assert state(e) == "running", state(e)
        machine.succeed(f"test -s /data/dev/jobs/{e}/outcome")
        proj(f"stop {e}")

    with subtest("an issue closed under a waiting job ends its waits"):
        d = f"/data/dev/jobs/{job}"
        machine.succeed(f"su - dev -c \"printf 'o/r#6\\t%s\\t{{}}\\n' $(date +%s) >{d}/await\"")
        github("repos/o/r/issues", "[]")
        machine.succeed("su - dev -c proj-inbox")
        machine.fail(f"test -e {d}/await")
        proj("_watch")
        time.sleep(3)
        assert machine.succeed(f"cat {d}/turns").strip() == "2"

    with subtest("a job stopped while GitHub is being asked stays stopped"):
        machine.succeed(f"su - dev -c \"printf 'o/r#6\\t%s\\t{{}}\\n' $(date +%s) >{d}/await\"")
        machine.succeed("touch /tmp/gh/slow")
        machine.succeed("su - dev -c 'proj _watch' >/tmp/watch.log 2>&1 &")
        time.sleep(1)
        proj(f"stop {job}")
        machine.wait_until_succeeds("! pgrep -u dev -f 'proj _watch'", timeout=60)
        machine.succeed("rm /tmp/gh/slow")
        machine.fail(f"test -e {d}/await")
        assert machine.succeed(f"cat {d}/turns").strip() == "2"

    with subtest("a turn started while proj stop waits for the lock is stopped too"):
        # What _watch does when there's news: start the job's next turn under its lock.
        machine.succeed(f"""cat >/tmp/turn.sh <<'EOF'
    export XDG_RUNTIME_DIR=/run/user/$(id -u)
    sleep 3
    systemd-run --user --unit="proj-$(systemd-escape {job}).service" sleep 600
    EOF""")
        machine.succeed(f"su - dev -c 'flock {d}/lock sh /tmp/turn.sh' >/tmp/turn.log 2>&1 &")
        time.sleep(1)
        proj(f"stop {job}")
        machine.succeed("grep -q 'Running as unit' /tmp/turn.log")
        machine.fail(f"systemctl --user -M dev@ -q is-active '{unit(job)}'")

    with subtest("a stop that fails still ends the job's waits, and says it failed"):
        machine.succeed(f"su - dev -c \"printf 'o/r#6\\t%s\\t{{}}\\n' $(date +%s) >{d}/await\"")
        machine.succeed(f"su - dev -c 'XDG_RUNTIME_DIR=/run/user/$(id -u dev) systemd-run --user -p RefuseManualStop=yes --unit=\"proj-$(systemd-escape {job}).service\" sleep 600'")
        machine.succeed(f"systemctl --user -M dev@ -q is-active '{unit(job)}'")
        rc, out = machine.execute(f"su - dev -c 'proj stop {job}' 2>&1")
        assert rc != 0 and "couldn't stop" in out, (rc, out)
        machine.fail(f"test -e {d}/await")
        machine.succeed(f"systemctl --user -M dev@ kill '{unit(job)}'")
        machine.wait_until_fails(f"systemctl --user -M dev@ -q is-active '{unit(job)}'", timeout=30)

    with subtest("a stop systemd can't be asked about fails, and still ends the waits"):
        machine.succeed(f"su - dev -c \"printf 'o/r#6\\t%s\\t{{}}\\n' $(date +%s) >{d}/await\"")
        machine.succeed(f"su - dev -c 'XDG_RUNTIME_DIR=/run/user/$(id -u dev) systemd-run --user --unit=\"proj-$(systemd-escape {job}).service\" sleep 600'")
        machine.succeed(f"systemctl --user -M dev@ -q is-active '{unit(job)}'")
        rc, out = machine.execute(f"su - dev -c 'env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR=/nonexistent proj stop {job}' 2>&1")
        assert rc != 0 and "couldn't stop" in out, (rc, out)
        machine.fail(f"test -e {d}/await")
        proj(f"stop {job}")
        machine.fail(f"systemctl --user -M dev@ -q is-active '{unit(job)}'")

    with subtest("a closed issue whose job can't be stopped doesn't hold up the others"):
        for i in (8, 9):
            github(f"repos/o/r/issues/{i}/comments", "[]")
        issues = [dict(issue, number=i, html_url=f"https://github.com/o/r/issues/{i}", body="t") for i in (8, 9)]
        github("repos/o/r/issues", json.dumps(issues))
        machine.succeed("su - dev -c proj-inbox")
        jobs = machine.succeed("su - dev -c 'proj-inbox status'")
        for j in jobs.split():
            if j.startswith("ib-"):
                machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
        # 8 closed, 9's answer still to post; systemd can't be asked, so 8's
        # job can't be stopped.
        github("repos/o/r/issues", json.dumps(issues[1:]))
        before = int(machine.succeed("date +%s"))
        machine.succeed("su - dev -c 'env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR=/nonexistent proj-inbox'")
        assert "did This task comes from GitHub issue o/r#9" in machine.succeed("cat /tmp/gh/posted")
        assert "o/r#8" in machine.succeed("su - dev -c 'proj-inbox status'")
        # Nothing else may come for o/r, so the stop is tried again in five minutes.
        wake = json.loads(machine.succeed("cat /var/lib/burrow/proj-inbox/wake.json"))
        assert before + 300 <= wake.get("o/r", 0) <= int(machine.succeed("date +%s")) + 300, wake
        machine.succeed("su - dev -c proj-inbox")
        assert "o/r#8" not in machine.succeed("su - dev -c 'proj-inbox status'")

    with subtest("an issue whose job dir was removed is taken up afresh"):
        listed = machine.succeed("su - dev -c 'proj-inbox status'")
        gone = next(x.split()[-1] for x in listed.splitlines() if x.startswith("o/r#9 "))
        machine.succeed(f"rm -rf /data/dev/jobs/{gone}")
        machine.succeed("su - dev -c proj-inbox")
        machine.succeed("su - dev -c proj-inbox")
        listed = machine.succeed("su - dev -c 'proj-inbox status'")
        again = next(x.split()[-1] for x in listed.splitlines() if x.startswith("o/r#9 "))
        assert again != gone, listed
        machine.wait_until_succeeds(f"su - dev -c 'proj state {again}' | grep -x done", timeout=120)

    with subtest("a repo out of reach is remembered until a pass reaches it again"):
        gone = "/var/lib/burrow/proj-inbox/.unreached"
        machine.succeed("mv /tmp/gh/repos_o_r_issues /tmp/gh/kept")
        machine.succeed("su - dev -c 'proj-inbox o/r'")
        machine.succeed(f"test -s {gone}/o_r")
        machine.succeed("mv /tmp/gh/kept /tmp/gh/repos_o_r_issues")
        # The pass that reaches everything again, whole (its shell has no
        # compgen, for one).
        out = machine.succeed("su - dev -c 'proj-inbox o/r' 2>&1")
        assert "command not found" not in out, out
        assert machine.succeed(f"ls -A {gone}").strip() == "", machine.succeed(f"ls -A {gone}")
        # One whose clone is gone is out of reach no more, after a full pass.
        machine.succeed(f"su - dev -c 'echo 0 >{gone}/sam_gone'")
        machine.succeed("su - dev -c proj-inbox")
        assert machine.succeed(f"ls -A {gone}").strip() == "", machine.succeed(f"ls -A {gone}")

    with subtest("a wait proj can't read is news, and doesn't stop _watch"):
        e = proj('run web "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        machine.succeed(f"su - dev -c 'echo https://example.com/nope >/data/dev/jobs/{e}/await'")
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "https://example.com/nope: not something proj can watch" in prompt, prompt
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        # A fingerprint that isn't one proj wrote is the same.
        machine.succeed(f"su - dev -c \"printf 'o/r#5\\t%s\\tnotjson\\n' $(date +%s) >/data/dev/jobs/{e}/await\"")
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 3 /data/dev/jobs/{e}/turns", timeout=30)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.3")
        assert "o/r#5: not something proj can watch" in prompt, prompt
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)

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
  '';
}
