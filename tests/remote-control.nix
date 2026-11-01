# A Remote Control server never restarts on a switch (the operator has to live
# through the switch it confirms), so claude-rc-scan catches servers up: one
# still on an old claude-rc-serve (its Claude Code, its permission mode)
# restarts as soon as none of its sessions is in use, and not before. An open
# session that has sat idle for half an hour isn't in use. Nothing waits for a
# timer: the switch scans, a session's status changing does,
# and a scan that leaves a server waiting books the next one for when its
# quiet spell ends. Only a project gets a server: a git checkout with an
# origin, or the machine's own flake, whatever it is. A clone made while the
# servers run gets one too.
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
    # The folders below with a session file are projects. Not projects: a git
    # repository with no origin, and linked worktrees of a project.
    imports = [
      self.nixosModules.remote-control
      (import ./remote-control/projects.nix {
        inherit pkgs;
        names = [ "idle" "busy" "recent" "waiting" "left" "late" "due" ];
        after = ''
          git init -q /data/dev/src/local
          git -C /data/dev/src/idle -c user.name=dev -c user.email=dev@example.invalid commit -q --allow-empty -m start
          git -C /data/dev/src/idle worktree add -q /data/dev/src/idle-worktrees/one
          git -C /data/dev/src/idle worktree add -q /data/dev/src/linked
        '';
      })
    ];
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
      "d /data/dev/src/due 0755 dev dev -"
      "f /data/dev/src/due/session 0644 dev dev - waiting 600"
      # The machine's own flake (burrow.flake), not a git checkout here.
      "d /data/dev/src/machine 0755 dev dev -"
      # Not projects: notes, and a folder of a project's extra checkouts.
      "d /data/dev/src/notes 0755 dev dev -"
      "d /data/dev/src/idle-worktrees 0755 dev dev -"
    ];
  };

  testScript = ''
    import re
    import time

    def started(project):
        return machine.succeed(f"cat /data/dev/src/{project}/started").split()

    def now():
        # The machine's clock, which Claude Code writes by and the scan reads
        # by: boot sets it apart from the host's, by up to a second.
        return time.time() + skew

    def session(project, status, ago):
        # What Claude Code writes when a session's status changes; returns
        # its statusUpdatedAt, in seconds.
        pid = machine.succeed(f"cat /data/dev/src/{project}/session.pid").strip()
        at = int(now() * 1000) - ago * 1000
        machine.succeed(f"echo '{{\"pid\":{pid},\"status\":\"{status}\",\"statusUpdatedAt\":{at}}}' >/home/dev/.claude/sessions/{pid}.json")
        return at / 1000

    def wake_in():
        # Seconds until the booked scan, or None.
        if machine.execute("systemctl --user -M dev@ is-active -q claude-rc-scan.timer")[0] != 0:
            return None
        out = machine.succeed("systemctl --user -M dev@ show -p NextElapseUSecMonotonic --value claude-rc-scan.timer").strip()
        at = sum(float(n) * {"h": 3600, "min": 60, "s": 1, "ms": 0.001}[u] for n, u in re.findall(r"([\d.]+)(h|min|ms|s)", out))
        return at - float(machine.succeed("cut -d' ' -f1 /proc/uptime"))

    machine.wait_for_unit("user@1001.service")
    sent = time.time()
    skew = float(machine.succeed("date +%s.%N")) - (sent + time.time()) / 2
    for p in ("idle", "busy", "recent", "waiting", "left", "late", "due", "machine"):
        machine.wait_until_succeeds(f"grep -qx old /data/dev/src/{p}/started", timeout=120)
    # Nothing is stale yet: no scan booked.
    machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=60)
    assert wake_in() is None, wake_in()
    booted = time.time()

    def state(template, folder):
        unit = machine.succeed(f"systemd-escape --template {template} {folder}").strip()
        return machine.succeed(f"systemctl --user -M dev@ show -p ActiveState --value {unit}").strip()

    with subtest("a folder that isn't a project gets no server"):
        for p in ("notes", "local", "linked", "idle-worktrees"):
            machine.fail(f"test -e /data/dev/src/{p}/started")
            assert state("claude-rc@.service", p) == "inactive", p
        # Each that may become one where it stands is watched for its origin;
        # a linked worktree, and a project, never are.
        for p in ("notes", "local", "idle-worktrees"):
            machine.wait_until_succeeds(f"systemctl --user -M dev@ is-active -q $(systemd-escape --template claude-rc-origin@.path {p})", timeout=30)
        for p in ("linked", "idle", "machine"):
            assert state("claude-rc-origin@.path", p) == "inactive", p

    machine.succeed("/run/current-system/specialisation/next/bin/switch-to-configuration test")

    with subtest("the switch scans: a stale server with no session restarts on the new one"):
        machine.wait_until_succeeds("grep -qx new /data/dev/src/idle/started", timeout=60)

    with subtest("a stale server whose session sat idle for an hour restarts"):
        machine.wait_until_succeeds("grep -qx new /data/dev/src/left/started", timeout=60)

    with subtest("a stale server whose session is working, or was lately, waits"):
        time.sleep(5)
        for p in ("busy", "recent", "waiting", "late", "due"):
            assert started(p) == ["old"], (p, started(p))

    with subtest("and a scan is booked for the soonest end of a quiet spell"):
        # recent, waiting and due went quiet 10 minutes before the servers
        # started.
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
        at = int(now() * 1000)
        machine.succeed(f"echo '{{\"pid\":{pid},\"status\":\"waiting\",\"statusUpdatedAt\":{at}}}' >/home/dev/.claude/status && cp /home/dev/.claude/status /home/dev/.claude/answer && mv /home/dev/.claude/status {f}")
        machine.succeed(f"cat /home/dev/.claude/answer >{fd} && kill {holder}")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/late/started", timeout=60)

    with subtest("a quiet spell that ends while a scan runs isn't missed"):
        # due's status is a link to a pipe, so a scan that reads it waits for
        # the test to write each read's answer. Nothing in the sessions
        # folder changes meanwhile: the link stays as it is. The scan held
        # began over 5 seconds before its answer, a spell that ended 2
        # seconds before it: for that scan due's server is held, and the end
        # of its spell already past. No timer is booked for a spell that's
        # over, and nothing changed on disk to start a scan: only the look
        # after the scan, at the time, sees it.
        pid = machine.succeed("cat /data/dev/src/due/session.pid").strip()
        f = f"/home/dev/.claude/sessions/{pid}.json"
        machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=60)
        machine.succeed(f"mkfifo -m 644 /home/dev/.claude/due && ln -s /home/dev/.claude/due /home/dev/.claude/link && mv -T /home/dev/.claude/link {f}")

        def answer(status, at, before=None):
            # A scan's read of due's status, held on the pipe, gets this:
            # one read at a time (status and statusUpdatedAt are a read
            # each), so it waits for the read before it to end and the next
            # one to come. Returns that read's pid.
            read = machine.wait_until_succeeds(f"pgrep -u dev -f 'jq .*sessions/{pid}.json' | grep -vx '{before}'", timeout=60).split()[0]
            machine.succeed(f"echo '{{\"pid\":{pid},\"status\":\"{status}\",\"statusUpdatedAt\":{at}}}' | timeout 10 tee /home/dev/.claude/due >/dev/null")
            machine.wait_until_fails(f"kill -0 {read}", timeout=10)
            return read

        # The scan held: began over 5 seconds before it's answered.
        machine.wait_until_succeeds(f"pgrep -u dev -f 'jq .*sessions/{pid}.json'", timeout=60)
        time.sleep(6)
        at = int((now() - 2 - 30 * 60) * 1000)
        read = answer("idle", at)
        read = answer("idle", at, read)
        # The scan after it reads the same, now past its spell's end.
        read = answer("idle", at, read)
        answer("idle", at, read)
        machine.wait_until_succeeds("grep -qx new /data/dev/src/due/started", timeout=60)
        machine.succeed(f"rm {f} /home/dev/.claude/due")

    with subtest("the booked scan comes when the quiet spell ends"):
        # The spell ends 30 seconds on: the scan this status starts books its
        # wake for then, however long that scan takes to run.
        ends = session("waiting", "waiting", 30 * 60 - 30) + 30 * 60
        deadline = time.time() + 25
        while (left := wake_in()) is None or left > ends - now() + 2:
            assert time.time() < deadline, (left, ends - now())
            time.sleep(0.5)
        assert started("waiting") == ["old"], started("waiting")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/waiting/started", timeout=90)

    with subtest("status changes one after another leave the scan's watchers watching"):
        # Each one its own scan, a dozen in a row: past the start limit (5 in
        # the hour this machine's manager gives).
        # A change while a scan runs only joins it, so each starts from none
        # running: the last scan's InvocationID, read with it inactive.
        def idle_invocation():
            return machine.wait_until_succeeds(
                "s=$(systemctl --user -M dev@ show -p ActiveState -p InvocationID claude-rc-scan.service) && "
                "echo \"$s\" | grep -qx ActiveState=inactive && echo \"$s\" | sed -n 's/^InvocationID=//p'", timeout=30).strip()
        for _ in range(12):
            last = idle_invocation()
            session("waiting", "idle", 0)
            machine.wait_until_succeeds(
                f"[ \"$(systemctl --user -M dev@ show -p InvocationID --value claude-rc-scan.service)\" != {last} ] && "
                "systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=10)
        machine.succeed("systemctl --user -M dev@ is-active -q claude-rc-scan-sessions.path claude-rc-scan.path")
        # A clone makes its folder before it writes the origin: the scan that
        # folder starts may find no origin yet, and the look after it does.
        machine.succeed("runuser -u dev -- ${pkgs.git}/bin/git init -q --bare /tmp/later.git")
        machine.succeed("runuser -u dev -- ${pkgs.git}/bin/git clone -q /tmp/later.git /data/dev/src/later")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/later/started", timeout=60)

    with subtest("a server already on the new one stays up, and nothing is booked"):
        session("waiting", "busy", 0)
        time.sleep(5)
        assert started("idle") == ["old", "new"], started("idle")
        machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-scan.service | grep -qx inactive", timeout=60)
        assert wake_in() is None, wake_in()

    with subtest("a folder that becomes a project long after it was made gets its server"):
        # Its config and then its origin come only once the scan the folder
        # started, and the look after it, are over and nothing else is left
        # to come: no scan running, booked or asked for. Then only the
        # folder's own config changing can bring the scan that serves it.
        def settled():
            machine.wait_until_succeeds(
                "s=$(systemctl --user -M dev@ show -p ActiveState -p InvocationID claude-rc-scan.service) && "
                "echo \"$s\" | grep -qx ActiveState=inactive && "
                "id=$(echo \"$s\" | sed -n 's/^InvocationID=//p') && "
                "a=$(systemctl --user -M dev@ show -p ActiveState -p InvocationID claude-rc-again@$id.service) && "
                "echo \"$a\" | grep -qx ActiveState=inactive && echo \"$a\" | grep -q '^InvocationID=.' && "
                "! systemctl --user -M dev@ list-jobs --no-legend | grep -q claude-rc && "
                "systemctl --user -M dev@ show -p InvocationID --value claude-rc-scan.service | grep -qx \"$id\"", timeout=60)
            assert wake_in() is None, wake_in()
        machine.succeed("mkdir /data/dev/src/slow && chown dev:dev /data/dev/src/slow")
        machine.wait_until_succeeds("systemctl --user -M dev@ is-active -q claude-rc-origin@slow.path", timeout=60)
        settled()
        machine.succeed("runuser -u dev -- ${pkgs.git}/bin/git init -q /data/dev/src/slow")
        settled()
        machine.fail("test -e /data/dev/src/slow/started")
        assert state("claude-rc@.service", "slow") == "inactive"
        machine.succeed("runuser -u dev -- ${pkgs.git}/bin/git -C /data/dev/src/slow remote add origin /tmp/later.git")
        machine.wait_until_succeeds("grep -qx new /data/dev/src/slow/started", timeout=60)
        # A project now, so its watch is over.
        machine.wait_until_succeeds("systemctl --user -M dev@ show -p ActiveState --value claude-rc-origin@slow.path | grep -qx inactive", timeout=60)
  '';
}
