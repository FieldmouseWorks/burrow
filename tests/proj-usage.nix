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
      machine.succeed("echo 30 >/tmp/spent && chmod 644 /tmp/spent")
      a = proj("run web spent")
      machine.wait_until_succeeds("test -e /data/dev/jobs/.usage.json", timeout=60)
      out = json.loads(proj("usage"))
      assert out["job"] == a and out["said"].startswith("You've hit your monthly spend limit"), out
      assert out["until"] > out["at"] + 20, out
      status = proj("status")
      assert status.startswith("Claude is out of usage until "), status
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
      assert machine.succeed(f"cat /data/dev/jobs/{a}/turns").strip() == "1"
      told = machine.succeed("cat /tmp/told-spent")
      assert told.startswith("Your last run was refused") and "monthly spend limit" in told and told.rstrip().endswith("spent"), told
      assert "--resume spent" in machine.succeed("cat /tmp/args")
      # Back: nothing held, said so, and the notice closed.
      assert proj("usage") == "", proj("usage")
      assert not proj("status").startswith("Claude is out of usage"), proj("status")
      notices = machine.succeed("cat /tmp/notices").splitlines()
      assert notices[-1].startswith("close proj-usage "), notices
      assert sum(n.startswith("open proj-usage") for n in notices) == 1, notices

  with subtest("proj usage back lets the line go before the reset: the owner raised the limit"):
      machine.succeed("echo 3600 >/tmp/spent")
      a = proj("run web spent")
      machine.wait_until_succeeds("test -e /data/dev/jobs/.usage.json", timeout=60)
      machine.succeed("rm /tmp/spent")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x queued", timeout=60)
      proj("usage back")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {a}' | grep -x done", timeout=120)
''
