# A Remote Control server never restarts on a switch (the operator has to live
# through the switch it confirms), so claude-rc-scan catches servers up: one
# still on an old claude-rc-serve (its Claude Code, its permission mode)
# restarts as soon as none of its sessions is in use, and not before. An open
# session that has sat idle for half an hour isn't in use. Nothing waits for a
# timer: the switch scans, a session's status changing does,
# and a scan that leaves a server waiting books the next one for when its
# quiet spell ends.
#
#   nix build .#checks.x86_64-linux.remote-control -L
{ self, pkgs }:
let
  # Claude's server, as far as claude-rc-scan can tell: one process, with a
  # child for each session, and Claude Code's ~/.claude/sessions/PID.json for
  # it. A project folder's `session` file holds one, as "STATUS SECONDS": what
  # it's doing, and since how long ago. Its pid goes in the folder's
  # `session.pid`. Each start notes which Claude it was in the folder's
  # `started`.
  claude =
    tag:
    pkgs.writeShellScriptBin "claude" ''
      echo ${tag} >>started
      if [ -e session ]; then
        read -r status ago <session
        sleep 99999 &
        echo $! >session.pid
        mkdir -p ~/.claude/sessions
        at=$(( $(${pkgs.coreutils}/bin/date +%s%3N) - ago * 1000 ))
        echo "{\"pid\":$!,\"status\":\"$status\",\"statusUpdatedAt\":$at}" >~/.claude/sessions/$!.json
      fi
      exec sleep infinity
    '';
in
pkgs.testers.runNixOSTest {
  name = "remote-control";

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.remote-control ];
    burrow = {
      claude.package = claude "old";
      remoteControl.enable = true;
    };
    # The same machine with a newer Claude Code: a new claude-rc-serve.
    specialisation.next.configuration.burrow.claude.package = lib.mkForce (claude "new");

    # The user manager's start limit, over an hour: a dozen scans in a row
    # pass it however slowly they come, unless the scan lifts its own.
    systemd.user.extraConfig = "DefaultStartLimitIntervalSec=1h";

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      # Logged in already, as far as claude-rc-serve checks.
      "f /home/dev/.claude.json 0600 dev dev - {}"
      "d /data/dev/src/idle 0755 dev dev -"
      "d /data/dev/src/busy 0755 dev dev -"
      "f /data/dev/src/busy/session 0644 dev dev - busy 7200"
      "d /data/dev/src/recent 0755 dev dev -"
      "f /data/dev/src/recent/session 0644 dev dev - idle 600"
      "d /data/dev/src/waiting 0755 dev dev -"
      "f /data/dev/src/waiting/session 0644 dev dev - waiting 600"
      "d /data/dev/src/left 0755 dev dev -"
      "f /data/dev/src/left/session 0644 dev dev - idle 3600"
      "d /data/dev/src/late 0755 dev dev -"
      "f /data/dev/src/late/session 0644 dev dev - idle 600"
    ];
  };

  testScript = ''
    import re
    import time

    def started(project):
        return machine.succeed(f"cat /data/dev/src/{project}/started").split()

    def session(project, status, ago):
        # What Claude Code writes when a session's status changes.
        pid = machine.succeed(f"cat /data/dev/src/{project}/session.pid").strip()
        at = int(time.time() * 1000) - ago * 1000
        machine.succeed(f"echo '{{\"pid\":{pid},\"status\":\"{status}\",\"statusUpdatedAt\":{at}}}' >/home/dev/.claude/sessions/{pid}.json")

    def wake_in():
        # Seconds until the booked scan, or None.
        if machine.execute("systemctl --user -M dev@ is-active -q claude-rc-scan.timer")[0] != 0:
            return None
        out = machine.succeed("systemctl --user -M dev@ show -p NextElapseUSecMonotonic --value claude-rc-scan.timer").strip()
        at = sum(float(n) * {"h": 3600, "min": 60, "s": 1, "ms": 0.001}[u] for n, u in re.findall(r"([\d.]+)(h|min|ms|s)", out))
        return at - float(machine.succeed("cut -d' ' -f1 /proc/uptime"))

    machine.wait_for_unit("user@1001.service")
    for p in ("idle", "busy", "recent", "waiting", "left", "late"):
        machine.wait_until_succeeds(f"grep -qx old /data/dev/src/{p}/started", timeout=120)
    # Nothing is stale yet: no scan booked.
    machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=60)
    assert wake_in() is None, wake_in()
    booted = time.time()

    machine.succeed("/run/current-system/specialisation/next/bin/switch-to-configuration test")

    with subtest("the switch scans: a stale server with no session restarts on the new one"):
        machine.wait_until_succeeds("grep -qx new /data/dev/src/idle/started", timeout=60)

    with subtest("a stale server whose session sat idle for an hour restarts"):
        machine.wait_until_succeeds("grep -qx new /data/dev/src/left/started", timeout=60)

    with subtest("a stale server whose session is working, or was lately, waits"):
        time.sleep(5)
        for p in ("busy", "recent", "waiting", "late"):
            assert started(p) == ["old"], (p, started(p))

    with subtest("and a scan is booked for the soonest end of a quiet spell"):
        # recent and waiting went quiet 10 minutes before the servers started.
        left = wake_in()
        soonest = 1200 - (time.time() - booted)
        assert left is not None and soonest - 300 < left < soonest + 5, (left, soonest)

    with subtest("a session's status changing scans: one quiet for an hour frees its server"):
        session("recent", "idle", 3600)
        machine.wait_until_succeeds("grep -qx new /data/dev/src/recent/started", timeout=60)

    with subtest("and so does a session ending"):
        pid = machine.succeed("cat /data/dev/src/busy/session.pid").strip()
        machine.succeed(f"kill {pid}; rm /home/dev/.claude/sessions/{pid}.json")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/busy/started", timeout=60)

    with subtest("a status written while a scan runs isn't missed"):
        # A scan held up reading waiting's status (a pipe, until the test
        # answers), after it has read late's. The path unit starts nothing
        # while it runs, so late's new status is the scan's own to see.
        pid = machine.succeed("cat /data/dev/src/waiting/session.pid").strip()
        f = f"/home/dev/.claude/sessions/{pid}.json"
        # Opening a pipe to read waits for a writer, so one holds it open:
        # the scan's read gets past opening it and waits for what's written.
        machine.succeed("mkfifo -m 644 /home/dev/.claude/hold")
        # Its output goes nowhere before it opens the pipe, which waits: the
        # test's own output stays its to close.
        holder = machine.succeed("{ exec sleep infinity >/home/dev/.claude/hold; } >/dev/null 2>&1 </dev/null & echo $!").strip()
        machine.succeed(f"mv /home/dev/.claude/hold {f}")
        jq = machine.wait_until_succeeds(f"pgrep -u dev -f 'jq .*sessions/{pid}.json'", timeout=60).split()[0]
        fd = machine.wait_until_succeeds(f"for d in /proc/{jq}/fd/*; do case $(readlink $d) in *sessions/{pid}.json*) echo $d;; esac; done | grep .", timeout=10).split()[0]
        session("late", "idle", 3600)
        # Waiting's status is an ordinary file again, and the read that's
        # held gets the same through the pipe it has open, then its end.
        at = int(time.time() * 1000)
        machine.succeed(f"echo '{{\"pid\":{pid},\"status\":\"waiting\",\"statusUpdatedAt\":{at}}}' >/home/dev/.claude/status && cp /home/dev/.claude/status /home/dev/.claude/answer && mv /home/dev/.claude/status {f}")
        machine.succeed(f"cat /home/dev/.claude/answer >{fd} && kill {holder}")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/late/started", timeout=60)

    with subtest("the booked scan comes when the quiet spell ends"):
        session("waiting", "waiting", 30 * 60 - 8)
        time.sleep(3)
        assert started("waiting") == ["old"], started("waiting")
        left = wake_in()
        assert left is not None and left < 10, left
        machine.wait_until_succeeds("grep -qx new /data/dev/src/waiting/started", timeout=60)

    with subtest("status changes one after another leave the scan's watchers watching"):
        # Each one its own scan, a dozen in a row: past the start limit (5 in
        # the hour this machine's manager gives).
        def invocation():
            return machine.succeed("systemctl --user -M dev@ show -p InvocationID --value claude-rc-scan.service").strip()
        for _ in range(12):
            last = invocation()
            session("waiting", "idle", 0)
            machine.wait_until_succeeds(
                f"[ \"$(systemctl --user -M dev@ show -p InvocationID --value claude-rc-scan.service)\" != {last} ] && "
                "systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=10)
        machine.succeed("systemctl --user -M dev@ is-active -q claude-rc-scan-sessions.path claude-rc-scan.path")
        machine.succeed("mkdir /data/dev/src/later && chown dev:dev /data/dev/src/later")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/later/started", timeout=60)

    with subtest("a server already on the new one stays up, and nothing is booked"):
        session("waiting", "busy", 0)
        time.sleep(5)
        assert started("idle") == ["old", "new"], started("idle")
        machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=60)
        assert wake_in() is None, wake_in()
  '';
}
