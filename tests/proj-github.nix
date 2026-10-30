# proj's GitHub side, as tests/proj.nix's script goes on, on its machine and
# its stand-ins for Claude and gh: that a job waiting on a PR or issue (proj
# await) is resumed when it changes, even before the turn ended and from
# proj-watch's own unit (which a turn's kicks reach for the repos it
# awaits), but never for proj-inbox's own comment, or once its deadline
# passes with GitHub unreadable, that an await GitHub can't read fails and
# awaits nothing, that a reply meanwhile owns the job's waits, that _watch and proj-inbox given a repo look at that repo alone and
# book when it next needs a look, that proj-inbox posts every finished
# turn's answer (one resumed before it looked too, none misread while a turn
# starts) and ends the waits of one whose issue closed (proj stop's, which a
# watch already asking GitHub keeps to), a stop that fails there not holding
# up the rest, that an issue a bot opened is the machine's work, its job
# acting as the machinery's App, and that a job dir removed or a wait it
# can't read stops neither proj-inbox nor _watch.
{ pkgs }:
''
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
      # The watchdog's to know past proj-watch's own deadline, which kicks
      # it first.
      since = int(machine.succeed(f"cut -f2 /data/dev/jobs/{e}/await").strip())
      w = waits()[e]
      assert w["kick"] == "proj-watch/o/r" and w["since"] == since and w["due"] == since + 24 * 3600 + 600, w
      # Its turn kicked what it was started to as it started and as it
      # ended, and proj-watch for o/r at its end.
      machine.wait_until_succeeds("grep -x /kick/proj-watch/o/r /tmp/kicks", timeout=30)
      assert sorted(kicks()) == ["/kick/autoland/o/r", "/kick/autoland/o/r", "/kick/proj-watch/o/r"], kicks()
      # A project folder's name is the job's, whatever it holds.
      machine.succeed("install -d -o dev -g dev '/data/dev/src/my web%n'")
      machine.succeed("truncate -s 0 /tmp/kicks")
      e2 = proj('run --kick autoland/o/q \"my web%n\" hello')
      machine.wait_until_succeeds(f"su - dev -c 'proj state \"{e2}\"' | grep -x done", timeout=120)
      machine.wait_until_succeeds("[ $(grep -cx /kick/autoland/o/q /tmp/kicks) = 2 ]", timeout=30)
      # A job's own --timeout stops its turn then, well before TIMEOUT, and
      # the stop still kicks what it was started to.
      machine.succeed("truncate -s 0 /tmp/kicks")
      e3 = proj('run --timeout 3s --kick autoland/o/t web "sleep 600"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e3}' | grep -x 'timed out after 3s'", timeout=60)
      machine.wait_until_succeeds("[ $(grep -cx /kick/autoland/o/t /tmp/kicks) = 2 ]", timeout=30)
      machine.fail("su - dev -c 'proj run --timeout soon web hello'")
      # One whose stream says nothing for its --idle is stuck, and stopped
      # then, kicking as before; one that keeps talking runs on past it.
      e4 = proj('run --idle 3s --kick autoland/o/i web "sleep 600"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e4}' | grep -x 'went quiet for 3s'", timeout=60)
      machine.wait_until_succeeds("[ $(grep -cx /kick/autoland/o/i /tmp/kicks) = 2 ]", timeout=30)
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

  with subtest("an await GitHub can't read fails, and awaits nothing"):
      e = proj('run web "sleep 1"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
      # o/r#99 isn't there: neither it nor the readable #5 is awaited.
      status, out = machine.execute(f"su - dev -c 'PROJ_JOB={e} proj await https://github.com/o/r/pull/5 https://github.com/o/r/pull/99' 2>&1")
      assert status == 1 and "couldn't read https://github.com/o/r/pull/99 from GitHub" in out, (status, out)
      machine.fail(f"test -e /data/dev/jobs/{e}/await.next")
      machine.fail(f"test -e /data/dev/jobs/{e}/end.next")
      machine.succeed(f"su - dev -c 'PROJ_JOB={e} proj await https://github.com/o/r/pull/5'")
      assert machine.succeed(f"cut -f3 /data/dev/jobs/{e}/await.next").strip().startswith("{"), machine.succeed(f"cat /data/dev/jobs/{e}/await.next")
      machine.succeed(f"rm /data/dev/jobs/{e}/await.next /data/dev/jobs/{e}/end.next")

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
      # Its turn's start and end kick the inbox's look at o/r, which posts
      # the answer.
      machine.wait_until_succeeds("[ $(grep -cx /kick/proj-inbox/o/r /tmp/kicks) = 2 ]", timeout=30)
      # Its next turn starts before the inbox looks again, as proj _watch's can.
      proj(f'reply {job} "sleep 1"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {job}' | grep -x done", timeout=120)
      machine.succeed("su - dev -c proj-inbox")
      posted = machine.succeed("cat /tmp/gh/posted")
      first = posted.find("did This task comes from GitHub issue o/r#7")
      assert "On it" in posted and -1 < first < posted.find("did sleep 1"), posted

  with subtest("an issue a bot opened is the machine's work, and acts as the machinery's App"):
      machine.succeed("su - dev -c 'git -C /data/dev/src/bt init -q && git -C /data/dev/src/bt remote add origin https://github.com/o/b'")
      bot = {"number": 3, "user": {"login": "sam-steward[bot]"}, "html_url": "https://github.com/o/b/issues/3",
             "title": "t", "body": "showenv", "state": "open", "labels": [{"name": "claude"}]}
      person = {**bot, "number": 4, "user": {"login": "sam"}, "html_url": "https://github.com/o/b/issues/4"}
      github("repos/o/b/issues", json.dumps([bot, person]))
      github("repos/o/b/issues/3/comments", "[]")
      github("repos/o/b/issues/4/comments", "[]")
      machine.succeed("su - dev -c 'proj-inbox o/b'")
      jobs = dict(line.split()[0::2] for line in machine.succeed("su - dev -c 'proj-inbox status'").splitlines() if line.startswith("o/b#"))
      for j in jobs.values():
          machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
      assert machine.succeed(f"cat /data/dev/jobs/{jobs['o/b#3']}/app").strip() == "workers"
      acts_as_workers(jobs["o/b#3"])
      machine.fail(f"test -e /data/dev/jobs/{jobs['o/b#4']}/app")
      assert "socket=none" in proj(f"log {jobs['o/b#4']}")
      github("repos/o/b/issues", "[]")
      machine.succeed("su - dev -c 'proj-inbox o/b'")

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

  with subtest("the inbox takes a hidden clone named for its repo, and no other hidden clone"):
      for d, repo, n in ((".github", ".github", 8), (".mirror", "m", 9)):
          machine.succeed(f"install -d -o dev -g dev /data/dev/src/{d}")
          machine.succeed(f"su - dev -c 'git init -q /data/dev/src/{d} && git -C /data/dev/src/{d} remote add origin https://github.com/o/{repo}'")
          issue = {"number": n, "user": {"login": "sam"}, "html_url": f"https://github.com/o/{repo}/issues/{n}",
                   "title": "t", "body": "sleep 1", "state": "open", "labels": [{"name": "claude"}]}
          github(f"repos/o/{repo}/issues", json.dumps([issue]))
          github(f"repos/o/{repo}/issues/{n}/comments", "[]")
      machine.succeed("su - dev -c 'proj-inbox o/.github'")
      machine.succeed("su - dev -c 'proj-inbox o/m'")
      looked = machine.succeed("su - dev -c 'proj-inbox status'")
      assert "o/.github#8" in looked and "o/m#9" not in looked, looked
''
