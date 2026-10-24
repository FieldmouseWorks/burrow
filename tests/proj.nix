# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead but can't starve a turn that has waited long, that a job
# whose turn has ended frees its slot, that one waiting on a PR or issue
# (proj await) is resumed when it changes, even before the turn ended and
# from proj-watch's own unit, but never for proj-inbox's own comment, or once
# its deadline passes with GitHub unreadable, that a reply meanwhile owns the
# job's waits, that proj-inbox posts every finished turn's answer (one
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
  # "linger N" answers, then stays up N seconds, as Claude does while
  # something it left in the background (a CI watch) is still going.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    case $prompt in
      sleep*) sleep "''${prompt#sleep }" ;;
      linger*)
        echo '{"type":"result","session_id":"s","result":"waiting on CI","is_error":false}'
        sleep "''${prompt#linger }"
        exit 0 ;;
      await*) proj await "''${prompt#await }" >/dev/null ;;
      # Awaits, then takes a while to answer: what changes meanwhile is news.
      slowawait*) proj await "''${prompt#slowawait }" >/dev/null; sleep 10 ;;
      eat) head -c 512M /dev/zero | tail >/dev/null; exit 1 ;;
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
        # Both run by hand here, never by their timers.
        watchEvery = "yearly";
        inbox = {
          enable = true;
          authors = [ "sam" ];
          every = "yearly";
        };
      };
    };
    environment.systemPackages = [ pkgs.git ];

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

    with subtest("the cap is on claude.slice"):
        out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax")
        assert "MemoryMax=" in out and "infinity" not in out, out

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
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        assert "[awaits o/r#5]" in proj("status"), proj("status")
        # proj-inbox's own answer on the PR isn't news.
        github("repos/o/r/pulls/5", '{"state": "open", "merged": false, "head": {"sha": "abc"}, "comments": 1, "review_comments": 0}')
        github("repos/o/r/issues/5/comments", '[{"id": 1, "body": "did it\\n<!-- proj-inbox -->"}]')
        proj("_watch")
        time.sleep(3)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}')
        # The timer's way, as a system unit running as the user.
        machine.succeed("systemctl start proj-watch.service")
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
        # No user manager to reach (a bus error, a restart): the turn can't start.
        machine.execute("su - dev -c 'XDG_RUNTIME_DIR=/nonexistent proj _watch'")
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        machine.succeed(f"test -s /data/dev/jobs/{e}/await")
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
        machine.succeed("su - dev -c proj-inbox")
        job = machine.succeed("su - dev -c 'proj-inbox status'").split()[-1]
        machine.wait_until_succeeds(f"su - dev -c 'proj state {job}' | grep -x done", timeout=120)
        # Its next turn starts before the inbox looks again, as proj _watch's can.
        proj(f'reply {job} "sleep 1"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {job}' | grep -x done", timeout=120)
        machine.succeed("su - dev -c proj-inbox")
        posted = machine.succeed("cat /tmp/gh/posted")
        first = posted.find("did This task comes from GitHub issue o/r#7")
        assert "On it" in posted and -1 < first < posted.find("did sleep 1"), posted

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
        machine.succeed("su - dev -c 'env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR=/nonexistent proj-inbox'")
        assert "did This task comes from GitHub issue o/r#9" in machine.succeed("cat /tmp/gh/posted")
        assert "o/r#8" in machine.succeed("su - dev -c 'proj-inbox status'")
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

    with subtest("a job past its memory cap is killed by its own limit"):
        e = proj("run web eat")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x failed", timeout=120)
        machine.succeed("journalctl -k | grep -q 'Memory cgroup out of memory'")
  '';
}
