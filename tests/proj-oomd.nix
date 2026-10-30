# systemd-oomd and proj's jobs, on real `proj run` units: what a job does
# against a memory limit of its own is that job's. A job stalled at its limit
# shows as pressure in every cgroup above it, while the machine has memory to
# spare, and a watch on any of them kills whichever job reclaims the most, here
# one doing nothing wrong. The control watches the way burrow.ops did before
# #191 (the root, user.slice and every slice in a user's manager) and loses
# that job (remi on 2026-10-29). On the guarded machine the stalled job and its
# siblings are left be, the stalled one past its cap dies by the kernel's hand
# without them, and oomd still kills what really drowns the machine, and only
# that.
#
#   nix build .#checks.x86_64-linux.proj-oomd -L
{ self, pkgs }:
let
  thrash = import ./thrash.nix { inherit pkgs; };
  # Claude, as far as proj can tell. "stall" waits for the test to give the job
  # a limit, then thrashes against it; "reclaim" gets through a sparse file at
  # its leisure, so its page cache keeps being scanned at far more pages a
  # second than a job kept asleep, while it hardly stalls; "idle" sleeps. Each
  # says what comes next first.
  claude = pkgs.writeShellScriptBin "claude" ''
    prompt=$(cat)
    proj done >/dev/null
    cd "$(mktemp -d -p /home/dev/work)"
    case $prompt in
      stall)
        until [ -e /home/dev/work/go ]; do sleep 1; done
        ${thrash}/bin/thrash ;;
      reclaim)
        ${pkgs.coreutils}/bin/truncate -s 4G sparse
        while :; do
          ${pkgs.coreutils}/bin/dd if=sparse of=/dev/null bs=1M count=64 skip=$((RANDOM % 4000)) status=none
          sleep 0.1
        done ;;
      idle) sleep infinity ;;
    esac
    ${pkgs.jq}/bin/jq -nc --arg r "did $prompt" '{type: "result", session_id: "s", result: $r, is_error: false}'
  '';
  # The machine's own hog (tests/oomd.nix): random bytes, kept.
  hog = pkgs.writers.writePython3 "hog" { } ''
    import os
    import time

    kept = []
    while True:
        kept.append(os.urandom(32 << 20))
        time.sleep(1)
  '';
  machine = {
    imports = [ self.nixosModules.proj ];
    burrow = {
      claude.package = claude;
      proj = {
        enable = true;
        minFreeMemory = 0;
        jobMemoryMax = "256M";
      };
    };
    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /data/dev/src/web 0755 dev dev -"
      "d /home/dev/work 0755 dev dev -"
    ];
    systemd.services.hog.serviceConfig.ExecStart = hog;
    virtualisation.memorySize = 1536;
    virtualisation.diskSize = 4096;
    virtualisation.cores = 2;
    # The test framework panics on any OOM, a cgroup's own included.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };
in
pkgs.testers.runNixOSTest {
  name = "proj-oomd";

  nodes.guarded = {
    imports = [ machine self.nixosModules.ops ];
    burrow.ops.enable = true;
    services.openssh.enable = true;
  };
  nodes.control = {
    imports = [ machine ];
    systemd.oomd = {
      enableRootSlice = true;
      enableSystemSlice = true;
      enableUserSlices = true;
    };
  };

  testScript = ''
    import shlex
    import time

    SLICE = "/user.slice/user-1001.slice/user@1001.service/claude.slice/claude-jobs.slice"

    def run(m, prompt):
        job = m.succeed(f"su - dev -c 'proj run web {prompt}'").strip()
        name = "proj-" + m.succeed(f"systemd-escape {job}").strip() + ".service"
        m.wait_until_succeeds(f"systemctl --user -M dev@ is-active {shlex.quote(name)}", timeout=60)
        return name

    def invocation(m, unit):
        return m.succeed(f"systemctl --user -M dev@ show -p InvocationID --value {shlex.quote(unit)}").strip()

    def cgroup(unit):
        return f"{SLICE}/{unit}"

    def marked(m, unit):
        # oomd's own word for each kill it makes.
        return m.execute(f"journalctl -o cat -u systemd-oomd | grep -qF 'Marked {cgroup(unit)} for killing'")[0] == 0

    def memstall(m, path):
        line = m.succeed("grep ^full " + shlex.quote(f"/sys/fs/cgroup{path}/memory.pressure"))
        return int(dict(f.split("=") for f in line.split()[1:])["total"])

    def stalled(m, path, seconds):
        # The share of the next `seconds` spent stalled on memory (tests/remote-control-oomd.nix).
        start, began = memstall(m, path), time.monotonic()
        time.sleep(seconds)
        return (memstall(m, path) - start) / (time.monotonic() - began) / 1e4

    def spare(m):
        return int(m.succeed("awk '/^MemAvailable:/ { print $2 }' /proc/meminfo")) >> 10

    start_all()
    for m in (guarded, control):
        m.wait_for_unit("user@1001.service")
        # A test VM drops swapDevices for its own, so make the swap here.
        m.succeed("fallocate -l 1G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile")

    jobs = {m: {p: run(m, p) for p in ("stall", "reclaim", "idle")} for m in (guarded, control)}
    before = {u: invocation(guarded, u) for u in jobs[guarded].values()}
    # Pressure without waiting for the cap: past MemoryHigh the kernel makes the
    # job sleep off every MiB it takes, which is all memory stall and next to
    # no scanning, what reclaim at a cap comes to once the job's cache is gone.
    for m in (guarded, control):
        unit = shlex.quote(jobs[m]["stall"])
        m.succeed(f"systemctl --user -M dev@ set-property --runtime {unit} MemoryHigh=48M")
        m.succeed("su - dev -c 'touch /home/dev/work/go'")

    with subtest("the control: one job stalled at its own cap gets another killed"):
        control.wait_until_succeeds(f"journalctl -o cat -u systemd-oomd | grep -F 'Marked {cgroup(jobs[control]['reclaim'])} for killing'", timeout=300)
        print(control.succeed("journalctl -o cat -u systemd-oomd | grep Marked"))

    with subtest("guarded, the same stall for longer than oomd's 30s, with memory to spare, kills nothing"):
        share = stalled(guarded, cgroup(jobs[guarded]["stall"]), 60)
        assert share > 80, share
        assert spare(guarded) > 256, spare(guarded)
        for unit in jobs[guarded].values():
            assert invocation(guarded, unit) == before[unit], unit
            assert not marked(guarded, unit), unit

    with subtest("guarded, past its cap the stalled job dies by the kernel's hand and takes no one with it"):
        unit = shlex.quote(jobs[guarded]["stall"])
        guarded.succeed(f"systemctl --user -M dev@ set-property --runtime {unit} MemoryHigh=infinity")
        guarded.wait_until_succeeds("journalctl -k | grep -q 'Memory cgroup out of memory'", timeout=120)
        assert not marked(guarded, jobs[guarded]["stall"])
        for prompt in ("reclaim", "idle"):
            unit = jobs[guarded][prompt]
            assert invocation(guarded, unit) == before[unit], unit

    with subtest("guarded, the machine really drowning: oomd kills what's drowning it, and no job"):
        guarded.succeed("systemctl start hog")
        guarded.wait_until_succeeds("journalctl -u hog | grep -q 'systemd-oomd killed'", timeout=600)
        guarded.fail("journalctl -k | grep -q 'Out of memory: Killed process'")
        for prompt in ("reclaim", "idle"):
            unit = jobs[guarded][prompt]
            assert invocation(guarded, unit) == before[unit], unit
            assert not marked(guarded, unit), unit
  '';
}
