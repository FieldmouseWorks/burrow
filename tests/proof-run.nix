# burrow-proof for real, in a user manager's unit as the steward starts it
# (crates/burrow-proof, start.rs), on a repo whose "GitHub" is a bare repo
# beside it: a pass runs its command inside a cgroup of its own, and what
# the command leaves (a child ignoring SIGTERM, one in a session of its own)
# is ended before its worktree goes; a run that outlasts its timeout ends
# there even when it's stuck before any command (a fetch that hangs,
# ignoring SIGTERM), with every process of it gone; a stop ends all of it
# too; a command killed is no verdict. However it ends, its worktrees go and
# end.json says how. The cgroup tests in the crate run its fences without
# systemd; this is systemd's half: RuntimeMaxSec, Delegate and ExecStopPost.
#
#   nix build .#checks.x86_64-linux.proof-run -L
{ self, pkgs }:
let
  proof = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proof}/bin/burrow-proof";
  git = "${pkgs.git}/bin/git";
  # A git whose fetch hangs, ignoring SIGTERM, as a stalled remote can.
  hangingGit = pkgs.writeShellScript "hanging-git" ''
    case " $* " in
      *" fetch "*) trap "" TERM; echo $$ >/home/dev/out/git; exec sleep 300 ;;
      *) exec ${git} "$@" ;;
    esac
  '';
in
pkgs.testers.runNixOSTest {
  name = "proof-run";

  nodes.machine = { ... }: {
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      # Its user manager runs without a login, as the steward's units need.
      linger = true;
    };
    # So what ignores SIGTERM is killed in seconds, not the default 90.
    systemd.user.extraConfig = "DefaultTimeoutStopSec=5s";
  };

  testScript = ''
    import json
    import shlex

    def dev(cmd):
        return machine.succeed(f"runuser -u dev -- env XDG_RUNTIME_DIR=/run/user/1001 bash -c {shlex.quote(cmd)}")

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1001.service")
    dev("""
      set -e
      g="${git} -c user.name=t -c user.email=t@t"
      cd /home/dev
      $g init -q --bare origin.git
      $g init -q -b main work
      cd work
      echo main >f && $g add f && $g commit -qm main && $g push -q ../origin.git HEAD:refs/heads/main
      echo pr >f && $g commit -qam pr && $g push -q ../origin.git HEAD:refs/pull/5/head
      $g rev-parse HEAD >/home/dev/head
      mkdir -p /home/dev/src /home/dev/state/builds /home/dev/proofs /home/dev/out
      $g clone -q /home/dev/origin.git /home/dev/src/r
    """)
    head = machine.succeed("cat /home/dev/head").strip()

    def started(id, runs, timeout=60, git="${git}"):
        rec = {"repo": "sam/r", "number": 5, "head": head, "checkout": "/home/dev/src/r", "runs": runs,
               "unit": f"autoland-{id}.service", "timeout": timeout, "bash": "${pkgs.bash}/bin/bash",
               "git": git, "trees": "/home/dev/proofs", "kick": None, "commentBytes": 60000}
        d = f"/home/dev/state/builds/{id}"
        dev(f"mkdir {d} && cat >{d}/build.json <<'EOF'\n{json.dumps(rec)}\nEOF")
        dev(f"${proof} start {d}")
        return d

    def ended(d):
        machine.wait_until_succeeds(f"test -e {d}/end.json", timeout=120)
        return json.loads(machine.succeed(f"cat {d}/end.json"))

    def gone(pid_file):
        pid = machine.succeed(f"cat /home/dev/out/{pid_file}").strip()
        machine.wait_until_fails(f"test -e /proc/{pid}", timeout=30)

    def nothing_left(id):
        machine.succeed("test -z \"$(ls -A /home/dev/proofs)\"")
        assert "/home/dev/proofs" not in dev("${git} -C /home/dev/src/r worktree list"), id
        machine.wait_until_fails(f"systemctl --user -M dev@ is-active autoland-{id}.service", timeout=30)

    with subtest("a pass runs in its fence, and what it leaves ends before its worktree goes"):
        d = started("proof-1", {"gate": " ".join([
            "cat /proc/self/cgroup >/home/dev/out/cgroup;",
            "(trap ''' TERM; exec sleep 300) & echo $! >/home/dev/out/a;",
            "setsid bash -c 'exec </dev/null >/dev/null 2>&1; trap \"\" TERM HUP; echo $$ >/home/dev/out/b; exec sleep 300' &",
            "sleep 1; echo SECRET-LOG-LINE; echo '## 6 of 6 passed' >\"$PROOF_SUMMARY\"",
        ])})
        end = ended(d)
        assert end["state"] == "done" and end["verdicts"] == {"gate": ["success", "Ran here and passed"]}, end
        cgroup = machine.succeed("cat /home/dev/out/cgroup")
        assert "/autoland-proof-1.service/check-0" in cgroup, cgroup
        gone("a")
        gone("b")
        comment = machine.succeed(f"cat {d}/comment.md")
        assert "## 6 of 6 passed" in comment and "SECRET-LOG-LINE" not in comment, comment
        assert "SECRET-LOG-LINE" in machine.succeed(f"cat {d}/log")
        nothing_left("proof-1")

    with subtest("past its timeout it ends, a fetch that hangs and ignores SIGTERM included"):
        d = started("proof-2", {"gate": "touch /home/dev/out/ran"}, timeout=3, git="${hangingGit}")
        end = ended(d)
        assert end["state"] == "timed out" and "verdicts" not in end, end
        gone("git")
        machine.fail("test -e /home/dev/out/ran")
        nothing_left("proof-2")

    with subtest("a stop ends all of it, and it ended short"):
        d = started("proof-3", {"gate": "(trap ''' TERM; exec sleep 300) & echo $! >/home/dev/out/c; touch /home/dev/out/started; wait"})
        machine.wait_until_succeeds("test -e /home/dev/out/started", timeout=60)
        dev("systemctl --user stop autoland-proof-3.service")
        end = ended(d)
        # Its child outlasting SIGTERM makes the stop's result a timeout.
        assert end["state"] in ("failed", "timed out") and "verdicts" not in end, end
        gone("c")
        nothing_left("proof-3")

    with subtest("a command killed is the machine failing, no verdict"):
        end = ended(started("proof-4", {"gate": "kill -9 $$"}))
        assert end["state"] == "failed" and "verdicts" not in end, end
        nothing_left("proof-4")
  '';
}
