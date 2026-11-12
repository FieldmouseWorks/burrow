# The machine out of Claude usage, for real, on proj/test.nix's machine and
# its stand-in for Claude: from 2026-11-01 to 11-04 every job's turn was
# refused ("You've hit your monthly spend limit"), each retried and reported
# as an error, the cause named nowhere. A refused run puts the machine out of
# usage until its limit resets: no turn takes a slot till then, the refused
# one waits in line and runs once more, its wait due a stall's time after the
# reset, `proj status` says why and until when, the owner hears once, and
# all of it carries on by itself after. proj's usage tests have the rest.
#
#   nix build .#checks.x86_64-linux.proj-usage -L
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; } "proj-usage" ''
  with subtest("a run refused for usage holds every turn till its limit resets, then each runs"):
      machine.succeed("echo 30 >/tmp/spent && chmod 644 /tmp/spent && touch /tmp/spent-stray")
      a = proj("run web spent")
      machine.wait_until_succeeds("test -e /data/dev/jobs/.usage.json", timeout=60)
      # What the refused run left is gone before its turn waits again: it
      # would hold the slot given up, or run beside the next run.
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x queued", timeout=60)
      machine.fail("pgrep -f proj-spent-stray")
      machine.succeed("rm /tmp/spent-stray")
      out = json.loads(proj("usage"))
      assert out["job"] == a and out["said"] == "Claude reached its weekly allowance", out
      assert out["until"] > out["at"] + 20, out
      status = proj("status")
      assert any(l.startswith("Claude is out of usage until ") for l in status.splitlines()), status
      notices = machine.succeed("cat /tmp/notices").splitlines()
      assert len(notices) == 1 and notices[0].startswith("open proj-usage Claude is out of usage until "), notices
      # The refused turn gave up its slot, and one started now takes none.
      machine.succeed("rm /tmp/spent")
      b = proj("run web hello")
      w = waits(lambda w: b in w and a in w)
      assert state(a) == "queued" and state(b) == "queued", (state(a), state(b))
      for j in (a, b):
          assert w[j]["due"] >= out["until"] + 60, w[j]
          assert "Claude is out of usage until" in w[j]["phase"], w[j]
      machine.succeed("sleep 5")
      assert state(a) == "queued" and state(b) == "queued", (state(a), state(b))
      # Reset: both run by themselves, the refused one told why, with what
      # it was asked, in its own conversation and turn.
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
      machine.wait_until_succeeds(f"su - dev -c 'proj state {b}' | grep -x done", timeout=120)
      turns = machine.succeed(f"cat /data/dev/jobs/{a}/turns /data/dev/jobs/{a}/*.outcome; ls /data/dev/jobs/{a}")
      assert turns.startswith("1\n"), turns
      told = machine.succeed("cat /tmp/told-spent")
      assert told.startswith("Your last run was refused") and "weekly allowance" in told and told.rstrip().endswith("spent"), told
      assert "monthly spend limit" not in told and "https://claude.ai/settings/usage" not in told, told
      assert "never ask the owner" in told, told
      assert "--resume spent" in machine.succeed("cat /tmp/spent-args"), machine.succeed("cat /tmp/spent-args")
      # Back: nothing held, said so, and the notice closed.
      assert proj("usage") == "", proj("usage")
      assert not any(l.startswith("Claude is out of usage") for l in proj("status").splitlines()), proj("status")
      notices = machine.succeed("cat /tmp/notices").splitlines()
      assert notices[-1].startswith("close proj-usage "), notices
      assert sum(n.startswith("open proj-usage") for n in notices) == 1, notices

  with subtest("proj usage back lets the line go before the reset: usage became available sooner"):
      machine.succeed("echo 3600 >/tmp/spent")
      a = proj("run web spent")
      # Its record outlives the last hold a while: what says it's out is
      # proj usage.
      machine.wait_until_succeeds("su - dev -c 'proj usage' | grep -q until", timeout=60)
      machine.succeed("rm /tmp/spent")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x queued", timeout=60)
      proj("usage back")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)

  with subtest("a usage-limit await an earlier proj wrote is the machine's hold now, and the job's next turn waits for it"):
      j = proj("run web hello")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=60)
      since = int(machine.succeed("date +%s"))
      reset = since + 20
      machine.succeed(f"su - dev -c \"printf 'usage-limit\\t{since}\\t{reset}\\n' >/data/dev/jobs/{j}/await\"")
      proj("_watch")
      assert json.loads(proj("usage"))["until"] == reset, proj("usage")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x queued", timeout=60)
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
      prompt = machine.succeed(f"cat /data/dev/jobs/{j}/prompt.2")
      assert "the usage limit your last turn ended on" in prompt, prompt
      assert proj("usage") == "", proj("usage")

  with subtest("proj usage back before an earlier proj's usage-limit await is taken in: that limit never holds"):
      j = proj("run web hello")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=60)
      since = int(machine.succeed("date +%s"))
      # The owner raised the limit, with no hold on for it to end.
      proj("usage back")
      machine.succeed(f"su - dev -c \"printf 'usage-limit\\t{since}\\t{since + 3600}\\n' >/data/dev/jobs/{j}/await\"")
      proj("_watch")
      assert proj("usage") == "", proj("usage")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -x done", timeout=120)
      machine.succeed(f"test -s /data/dev/jobs/{j}/prompt.2")
''
