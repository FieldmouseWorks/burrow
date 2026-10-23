# proj's limits, for real: a machine with proj on and a stand-in for Claude
# (burrow.claude.package) checks that jobs past maxJobs wait in line, that
# --first goes ahead, and that a job past its memory cap is killed by its own
# cgroup's limit rather than the machine running out.
#
#   nix build .#checks.x86_64-linux.proj -L
{ self, pkgs }:
let
  # Claude, as far as proj can tell: reads the prompt, answers in stream-json.
  # "sleep N" sleeps; "eat" holds far more memory than the test's job cap.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    case $prompt in
      sleep*) sleep "''${prompt#sleep }" ;;
      eat) head -c 512M /dev/zero | tail >/dev/null; exit 1 ;;
    esac
    echo '{"type":"result","session_id":"s","result":"did '"$prompt"'","is_error":false}'
  '';
in
pkgs.testers.runNixOSTest {
  name = "proj";

  nodes.machine = {
    imports = [ self.nixosModules.proj ];
    burrow = {
      claude.package = claude;
      proj = {
        enable = true;
        maxJobs = 2;
        # The VM is small: only the slot count holds jobs back here.
        minFreeMemory = 0;
        jobMemoryMax = "128M";
      };
    };

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [ "d /data/dev/src/web 0755 dev dev -" ];
    virtualisation.memorySize = 2048;
  };

  testScript = ''
    import time

    def proj(args):
        return machine.succeed(f"su - dev -c 'proj {args}'").strip()

    def state(job):
        return proj(f"state {job}")

    def unit(job):
        return "proj-" + machine.succeed(f"systemd-escape {job}").strip() + ".service"

    def started(job):
        return float(machine.succeed(f"stat -c %.9Y /data/dev/jobs/{job}/slot"))

    machine.wait_for_unit("user@1001.service")

    with subtest("the cap is on claude.slice"):
        out = machine.succeed("systemctl --user -M dev@ show claude.slice -p MemoryHigh -p MemoryMax")
        assert "MemoryMax=" in out and "infinity" not in out, out

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
        out = machine.succeed(f"systemctl --user -M dev@ show {unit(a)} -p Slice -p MemoryMax")
        assert "Slice=claude-jobs.slice" in out and "MemoryMax=134217728" in out, out
        proj(f"stop {a}")
        proj(f"stop {b}")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {c}' | grep -x done", timeout=120)
        machine.wait_until_succeeds(f"su - dev -c 'proj state {d}' | grep -x done", timeout=120)
        assert started(d) < started(c), (started(d), started(c))

    with subtest("a job past its memory cap is killed by its own limit"):
        e = proj("run web eat")
        machine.wait_until_succeeds(f"su - dev -c 'proj state {e}' | grep -x failed", timeout=120)
        machine.succeed("journalctl -k | grep -q 'Memory cgroup out of memory'")
  '';
}
