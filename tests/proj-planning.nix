# Planning-on integration complements the existing lifecycle fixture's
# planning-off coverage and the Rust sequencer's failure/retry tests.
{ self, pkgs }:
import ./proj/test.nix { inherit self pkgs; planning = true; } "proj-planning" ''
  with subtest("a labelled human task has a bounded fresh planning session before its worker"):
      import shlex
      machine.succeed("su - dev -c 'git -C /data/dev/src/ib init -q && git -C /data/dev/src/ib remote add origin https://github.com/o/r'")
      machine.succeed("mkdir -p /tmp/gh && chmod 777 /tmp/gh && install -m 666 /dev/null /tmp/gh/posted && touch /tmp/inbox-hold")
      task = {"number": 7, "user": {"login": "sam", "id": 1, "type": "User"}, "html_url": "https://github.com/o/r/issues/7",
              "repository_url": "https://api.github.com/repos/o/r", "title": "bounded task",
              "body": "inbox-done " + "x" * 70000, "state": "open", "labels": [{"name": "claude"}]}
      for endpoint, data in [("repos_o_r_issues", [task]), ("repos_o_r_issues_7", task), ("repos_o_r_issues_7_comments", [])]:
          machine.succeed("printf %s " + shlex.quote(json.dumps(data)) + " >/tmp/gh/" + endpoint + " && chmod 666 /tmp/gh/" + endpoint)
      machine.succeed("su - dev -c 'proj-inbox o/r'")
      planner = machine.succeed("cat /var/lib/burrow/proj-inbox/.triage/o_r-7/job").strip()
      status = machine.succeed("su - dev -c 'proj-inbox status'").strip()
      assert status.endswith(planner) and "sorting" in status, status
      machine.fail("test -e /var/lib/burrow/proj-inbox/o_r-7/job")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {planner}' | grep -x done", timeout=120)
      assert machine.succeed(f"cat /data/dev/jobs/{planner}/role").strip() == "planner"
      assert machine.succeed(f"wc -c </data/dev/jobs/{planner}/prompt.1").strip().isdigit()
      assert int(machine.succeed(f"wc -c </data/dev/jobs/{planner}/prompt.1")) < 66000
      assert "inbox-done " + "x" * 70000 in machine.succeed(f"cat /data/dev/jobs/{planner}/task-source")
      machine.succeed("su - dev -c 'proj-inbox o/r'")
      machine.succeed("su - dev -c 'proj-inbox o/r'")
      worker = machine.succeed("su - dev -c 'proj-inbox status'").split()[-1]
      assert worker != planner
      machine.succeed("rm /tmp/inbox-hold")
      machine.wait_until_succeeds(f"su - dev -c 'proj state {worker}' | grep -x done", timeout=120)
      assert machine.succeed(f"cat /data/dev/jobs/{worker}/turns").strip() == "1"
      assert machine.succeed(f"cat /data/dev/jobs/{planner}/session").strip() == "planner"
      assert machine.succeed(f"cat /data/dev/jobs/{worker}/session").strip() == "s"
      assert "Plan the size" not in machine.succeed(f"cat /data/dev/jobs/{worker}/prompt.1")
''
