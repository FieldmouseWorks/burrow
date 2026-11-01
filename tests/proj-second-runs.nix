# A turn's second run, for real, on proj/test.nix's machine and its stand-in
# for Claude: that a job stopped after its answer with work in the background
# has the rest of that run stopped, hears so once and answers again, what the
# first run left in its own session gone before the second starts; that the
# turn ends as its last run did, its log saying so even when its unit went
# first; and that the clearing up between runs counts against the turn's one
# timeout. The rest of a job's turns is proj.nix's.
#
#   nix build .#checks.x86_64-linux.proj-second-runs -L
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; } "proj-second-runs" ''
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
''
