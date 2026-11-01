# proj's slots, for real, on proj/test.nix's machine and its stand-in for
# Claude: that jobs past maxJobs wait in line, that --first goes ahead but
# can't starve a turn that has waited long, that a pipeline job's every turn,
# in one of the pipeline's own repos, takes the slot kept for them when the
# rest are full, and one in a lead repo goes ahead of the others, while one
# anywhere else goes as a steward job and says so (proj's pick tests have how
# origins are told apart), that a job run --unstick takes a slot of its own,
# every turn of it, while every other one is held and turns wait in line, and
# waits only for another --unstick turn (one made so later, by proj unstick,
# too: waiting, or at its next turn), that proj withdraw ends a job only
# while its turn waits for a slot (one that has started, or ended, it leaves
# be, saying so, and one from an older proj it refuses), that a turn takes a
# free slot only once its job's lock, which withdraw looks under, is free,
# that a job whose turn has ended frees its slot, and that a turn kicks what
# it was started to once it takes its slot, not while it waits in line.
#
#   nix build .#checks.x86_64-linux.proj-slots -L
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; } "proj-slots" ''
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
      # A turn from an older proj never says it may be withdrawn, and
      # isn't: it might take its slot unlocked.
      machine.succeed(f"mv /data/dev/jobs/{c}/1.withdrawable /tmp/withdrawable")
      machine.fail(f"su - dev -c 'proj withdraw {c}'")
      assert state(c) == "queued", state(c)
      machine.succeed(f"mv /tmp/withdrawable /data/dev/jobs/{c}/1.withdrawable")
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

  with subtest("a pipeline job anywhere else goes as a steward job, and says so in proj status and its turn's journal"):
      q = proj('run --pipeline ib "sleep 1"')
      demoted = "asked as --pipeline, but its project has no origin: it goes as a steward job"
      listed = proj("status")
      assert f"[{demoted}]" in listed, listed
      # A line a job, whatever columns it has: each starts with its id.
      jobs = machine.succeed("ls -1 /data/dev/jobs").splitlines()
      assert all(any(line.startswith(j + " ") for j in jobs) for line in listed.splitlines()), listed
      machine.wait_until_succeeds(f"su - dev -c 'proj state {q}' | grep -x done", timeout=120)
      machine.wait_until_succeeds(f"journalctl -q '_SYSTEMD_USER_UNIT={unit(q)}' | grep -qF \"proj: {demoted}\"", timeout=30)
''
