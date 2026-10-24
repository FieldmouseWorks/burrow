# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead but can't starve a turn that has waited long, that a job
# whose turn has ended frees its slot, that one waiting on a PR (proj await)
# is resumed when the PR changes (even if it changed before the turn ended)
# or once its deadline passes with GitHub unreadable, and that a job past its
# memory cap is
# killed by its own cgroup's limit rather than the machine running out.
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
    esac
    ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'
  '';
  # GitHub, as far as proj-watch can tell: each endpoint's answer is a file in
  # /tmp/gh that the test writes.
  gh = pkgs.writeShellScriptBin "gh" ''
    [ "$1" = api ] || exit 2
    ep=''${2%%\?*}
    cat "/tmp/gh/''${ep//\//_}"
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
      };
    };

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [ "d /data/dev/src/web 0755 dev dev -" ];
    virtualisation.memorySize = 2048;
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what's checked here.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };

  testScript = ''
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def unit(job):
        return "proj-" + machine.succeed(f"systemd-escape {job}").strip() + ".service"

    def started(job):
        return float(machine.succeed(f"stat -c %.9Y /data/dev/jobs/{job}/slot"))

    machine.wait_for_unit("user@1001.service")

    with subtest("the cap is on claude.slice"):
        out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax")
        assert "MemoryMax=" in out and "infinity" not in out, out

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
        e = proj('run web "await https://github.com/o/r/pull/5"')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        assert "[awaits o/r#5]" in proj("status"), proj("status")
        proj("_watch")
        time.sleep(3)
        assert machine.succeed(f"cat /data/dev/jobs/{e}/turns").strip() == "1"
        github("repos/o/r/commits/abc/check-runs", '{"check_runs": [{"name": "ci", "status": "completed", "conclusion": "success"}]}')
        proj("_watch")
        machine.wait_until_succeeds(f"grep -x 2 /data/dev/jobs/{e}/turns", timeout=30)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
        prompt = machine.succeed(f"cat /data/dev/jobs/{e}/prompt.2")
        assert "o/r#5: checks" in prompt and "ci=success" in prompt, prompt
        assert "awaits" not in proj("status"), proj("status")

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

    with subtest("a job past its memory cap is killed by its own limit"):
        e = proj("run web eat")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x failed", timeout=120)
        machine.succeed("journalctl -k | grep -q 'Memory cgroup out of memory'")
  '';
}
