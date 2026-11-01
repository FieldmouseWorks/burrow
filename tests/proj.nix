# proj's turns, for real, on proj/test.nix's machine and its stand-in for
# Claude: that Claude has no ceiling of its own, that a turn that failed
# without a word has no answer, that a job stopped after its answer with work
# in the background has the rest of that run stopped, hears so once and
# answers again (the turn ending as its last run did, its log saying so even
# when its unit went first, and within one timeout that the clearing up
# between runs counts against), that a reply while the last turn's unit is
# still ending waits for it before its turn is written, that every turn says
# what comes next (proj await, ask or done), one that doesn't hearing so once
# and, still silent, being the stall watchdog's at once (one that ended in a
# failure a grace from its end, even killed with a log hours old, and a turn
# in line or running to its deadline, the watchdog kicked as they change),
# while a question, a finished job, a stopped one and an --owned job's end
# aren't, that a job run --app acts as its App every turn, whatever the login
# sets and if the App goes while it waits, or not at all, and that a job past
# its memory cap is killed by its own cgroup's limit rather than the machine
# running out, that what's killed is the command that outgrew it, the turn
# carrying on to its answer, that a run killed for memory before it answers
# (Claude itself, as a rule) is carried on in its own session, twice a turn
# at most, that a turn killed before it answers can still be continued, in
# its own session, and that each job has a scratch dir of its own, gone with
# the worktrees in it once the job is over (done, stopped, or an --owned
# job's turn that awaits nothing) and kept while it asks (proj's scratch
# tests have the rest). Who waits in line for a slot is proj-slots.nix's,
# and proj's GitHub side proj-github.nix's.
#
#   nix build .#checks.x86_64-linux.proj -L
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; } "proj" ''
  with subtest("Claude has no ceiling and no throttle of its own, and may swap"):
      # The kernel shares the machine's memory out: a cap here only killed
      # or stalled Claudes while memory sat free.
      out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax -p MemorySwapMax")
      assert sorted(out.split()) == ["MemoryHigh=infinity", "MemoryMax=infinity", "MemorySwapMax=infinity"], out

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
      # Resumed, it puts out an empty result first and then gets to work:
      # that's no answer, so the run isn't stopped ten seconds on.
      machine.succeed("echo resumed >/tmp/cut-mode")
      c = proj('run web "linger 600"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
      assert proj(f"log {c}") == "waiting on CI\n\nthe build passed", proj(f"log {c}")
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
      # Anyone else's failure is a stall once nothing has resumed it.
      f = proj("run web fail")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x error", timeout=120)
      waits(lambda w: w.get(f, {}).get("phase") == "turn 1 ended (error), and nothing resumed it")
      proj(f"stop {f}")
      assert f not in waits(), waits()
      # Killed whole with nothing written, as systemd-oomd does, its log
      # long quiet: the grace counts from the kill, which _ended saw from
      # outside the cgroup.
      k = proj('run web "sleep 600"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {k}' | grep -x running", timeout=120)
      machine.succeed(f"su - dev -c 'touch -d \"1 hour ago\" /data/dev/jobs/{k}/1.jsonl /data/dev/jobs/{k}/prompt.1'")
      before = int(machine.succeed("date +%s"))
      machine.succeed(f"systemctl --user -M dev@ kill --kill-whom=all --signal=KILL '{unit(k)}'")
      w = waits(lambda w: "nothing resumed it" in w.get(k, {}).get("phase", ""))[k]
      assert w["since"] >= before and w["due"] == w["since"] + 120, w
      machine.succeed(f"test -e /data/dev/jobs/{k}/1.ended")
      # And the next turn, running, has no end yet.
      machine.succeed(f"su - dev -c 'echo s >/data/dev/jobs/{k}/session'")
      proj(f'reply {k} "sleep 600"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {k}' | grep -x running", timeout=120)
      machine.fail(f"test -e /data/dev/jobs/{k}/2.ended")
      proj(f"stop {k}")

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

  with subtest("a job run --since dates from then, and only a time not after now will do"):
      s = proj("run --since 1700000000 web silent")
      machine.succeed(f"grep -x 1700000000 /data/dev/jobs/{s}/since")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {s}' | grep -x done", timeout=120)
      machine.fail("su - dev -c 'proj run --since soon web silent'")
      machine.fail("su - dev -c 'proj run --since 9999999999 web silent'")
      machine.fail(f"su - dev -c 'proj reply --since 1700000000 {s} silent'")

  with subtest("a job run --app acts as that App every turn, or not at all"):
      e = proj("run --app workers web showenv")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
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

  with subtest("an earlier turn's answer is read without the turn count"):
      e = proj('run web "sleep 1"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
      machine.succeed(f"mv /data/dev/jobs/{e}/turns /tmp/turns-aside")
      assert proj(f"log {e} --turn 1") == "did sleep 1"
      machine.succeed(f"mv /tmp/turns-aside /data/dev/jobs/{e}/turns")

  with subtest("every answer in a turn is kept, not only the last"):
      f = proj("run web twice")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x done", timeout=120)
      assert proj(f"log {f}") == "the answer\n\ndid twice", proj(f"log {f}")
      assert machine.succeed(f"cat /data/dev/jobs/{f}/1.result") == "the answer\n\ndid twice\n"
      # The answer after the first, read and dropped, isn't a broken pipe in
      # the turn's journal.
      machine.fail(f"journalctl -q '_SYSTEMD_USER_UNIT={unit(f)}' | grep -F 'Broken pipe'")

  with subtest("an answer, then an error: the log has both"):
      f = proj("run web answerfail")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {f}' | grep -x error", timeout=120)
      log = proj(f"log {f}")
      assert log == 'the answer\n\n(turn 1 ended in an error)\n["refused"]', log

  with subtest("a job past its memory cap is killed by its own limit, and carries on"):
      e = proj("run web eat")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x done", timeout=120)
      machine.succeed("journalctl -k | grep -q 'Memory cgroup out of memory'")
      assert "--resume eat" in machine.succeed("cat /tmp/args"), machine.succeed("cat /tmp/args")

  with subtest("a run whose Claude outgrows its cap carries on in its own session, twice a turn at most"):
      b = proj("run web bloat")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x done", timeout=120)
      assert "--resume bloat" in machine.succeed("cat /tmp/args"), machine.succeed("cat /tmp/args")
      assert proj(f"log {b}").startswith("did Your last run was killed for memory"), proj(f"log {b}")
      # Killed each time it's carried on: the third kill ends the turn.
      machine.succeed("touch /tmp/again")
      b = proj("run web bloat")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x failed", timeout=180)
      machine.succeed("rm /tmp/again")
      runs = machine.succeed(f"grep -c '\"session_id\":\"bloat\"' /data/dev/jobs/{b}/1.jsonl").strip()
      assert runs == "3", runs

  with subtest("a turn killed before it answers can still be continued, in its own session"):
      # As its unit goes when systemd-oomd kills it.
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

  with subtest("a job's scratch is its own, and goes with its worktrees once the job is over"):
      # web as a git repo, with a commit to check out.
      machine.succeed("su - dev -c 'git -C /data/dev/src/web init -q && git -C /data/dev/src/web -c user.name=t -c user.email=t@t commit -q --allow-empty -m one'")

      def trees():
          return machine.succeed("su - dev -c 'git -C /data/dev/src/web worktree list --porcelain'").count("worktree ")

      def cleared(job):
          # Its dir goes at once (renamed aside), what was in it and its
          # worktree's registration on web only after: wait for all three.
          machine.wait_until_succeeds(
              f"! test -e /data/dev/scratch/jobs/{job}"
              " && test -z \"$(find /data/dev/scratch/jobs -maxdepth 1 -name '.gone-*')\""
              " && test \"$(su - dev -c 'git -C /data/dev/src/web worktree list --porcelain' | grep -c '^worktree ')\" = 1",
              timeout=60,
          )

      a = proj("run web scratch")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
      assert machine.succeed("cat /tmp/scratch").strip() == f"/data/dev/scratch/jobs/{a}", machine.succeed("cat /tmp/scratch")
      assert f"/data/dev/scratch/jobs/{a} (" in machine.succeed("cat /tmp/args"), machine.succeed("cat /tmp/args")
      # Done: the turn's end clears it, its worktree off web with it.
      cleared(a)
      # Asking, it keeps it for the reply; stopped, it's over.
      q = proj('run web "scratch ask"')
      machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x done", timeout=120)
      assert end(q) == "ask", end(q)
      machine.succeed(f"test -e /data/dev/scratch/jobs/{q}/web/built")
      assert trees() == 2, trees()
      proj(f"stop {q}")
      cleared(q)
      # An --owned job's turn that awaits nothing is its last.
      machine.succeed("touch /tmp/silent")
      o = proj("run --owned --kick autoland/o/w web scratch")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {o}' | grep -x done", timeout=120)
      cleared(o)
      machine.succeed("rm /tmp/silent")
''
