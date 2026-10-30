# systemd-oomd (burrow.ops.oomd) never kills a Remote Control server, which
# would take every session on it: a session drowning its server in memory
# pressure is left to the kernel, which kills that session alone at the
# server's cap. The control watches pressure the way burrow.ops did before
# (the root, user.slice and every slice in a user's manager, NixOS's own
# options) with no opt-out on the server, and loses the whole server to oomd.
#
#   nix build .#checks.x86_64-linux.remote-control-oomd -L
{ self, pkgs }:
let
  # Grows by a MiB at a time until it's killed. Between steps it reads its
  # own 4 MiB file back, so its cgroup always has page cache to reclaim:
  # oomd only acts on pressure that comes with reclaim.
  thrash = pkgs.writeCBin "thrash" ''
    #include <fcntl.h>
    #include <stdlib.h>
    #include <string.h>
    #include <unistd.h>
    static char buf[1 << 20];
    int main(void) {
      int fd = open("fill", O_RDWR | O_CREAT | O_TRUNC, 0600);
      if (fd < 0) return 1;
      memset(buf, 1, sizeof buf);
      for (int i = 0; i < 4; i++)
        if (write(fd, buf, sizeof buf) != sizeof buf) return 1;
      fsync(fd);
      for (;;) {
        char *p = malloc(1 << 20);
        if (!p) return 1;
        memset(p, 1, 1 << 20);
        for (off_t o = 0; pread(fd, buf, sizeof buf, o) > 0; o += sizeof buf)
          ;
      }
    }
  '';
  # Claude's server, as far as oomd and the cap can tell: one process with two
  # sessions. The sibling sleeps; the other waits for a line on the folder's
  # `go`, then thrashes. The server notes each session's pid (NAME.pid) and
  # how it ended (NAME.exit).
  claude = pkgs.writeShellScriptBin "claude" ''
    declare -A names
    sleep infinity &
    names[$!]=sibling
    echo $! >sibling.pid
    { read -r _ <go; exec ${thrash}/bin/thrash; } &
    names[$!]=thrash
    echo $! >thrash.pid
    while wait -n -p ended; status=$?; [ -n "''${ended:-}" ]; do
      echo $status >"''${names[$ended]}.exit"
      ended=
    done
    exec sleep infinity
  '';
  machine = {
    imports = [ self.nixosModules.remote-control ];
    burrow = {
      claude.package = claude;
      remoteControl = {
        enable = true;
        servers.busy.memoryMax = "256M";
      };
    };
    # Pressure without waiting for the cap: past MemoryHigh the kernel makes
    # the session sleep off every MiB it takes, which is all memory stall.
    systemd.user.services."claude-rc@busy".serviceConfig.MemoryHigh = "48M";
    environment.systemPackages = [ pkgs.attr ];

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      # Logged in already, as far as claude-rc-serve checks.
      "f /home/dev/.claude.json 0600 dev dev - {}"
      "d /data/dev/src/busy 0755 dev dev -"
      "p /data/dev/src/busy/go 0600 dev dev -"
    ];
    # The test framework panics on any OOM, a cgroup's own included; a real
    # machine kills inside the cgroup, which is what the guarded one waits for.
    boot.kernel.sysctl."vm.panic_on_oom" = 0;
  };
in
pkgs.testers.runNixOSTest {
  name = "remote-control-oomd";

  nodes.guarded = {
    imports = [ machine self.nixosModules.ops ];
    burrow.ops.enable = true;
  };
  nodes.control = { lib, ... }: {
    imports = [ machine ];
    systemd.oomd = {
      enableRootSlice = true;
      enableSystemSlice = true;
      enableUserSlices = true;
    };
    systemd.user.services."claude-rc@".serviceConfig.ManagedOOMPreference = lib.mkForce "none";
  };

  testScript = ''
    import time

    UNIT = "claude-rc@busy.service"
    DIR = "/data/dev/src/busy"

    def show(m, *props):
        out = m.succeed(f"systemctl --user -M dev@ show {UNIT} " + " ".join(f"-p {p}" for p in props))
        return dict(line.split("=", 1) for line in out.splitlines())

    def pid(m, session):
        return m.succeed(f"cat {DIR}/{session}.pid").strip()

    def alive(m, p):
        return m.execute(f"kill -0 {p}")[0] == 0

    def server(m):
        # The stand-in's pid: the pane of the unit's tmux.
        return m.succeed(f"pgrep -P {show(m, 'MainPID')['MainPID']}").strip()

    def full(m, cgroup):
        # The share of the last 10s all of the cgroup's busy tasks spent
        # stalled on memory: what oomd's pressure watches read.
        line = m.succeed(f"grep ^full /sys/fs/cgroup{cgroup}/memory.pressure")
        return float(line.split()[1].removeprefix("avg10="))

    def monitored(m, section):
        """The cgroups oomctl lists under one section (tests/oomd.nix)."""
        paths, inside = set(), False
        for line in m.succeed("oomctl").splitlines():
            if not line[:1].isspace():
                inside = line.strip() == section + ":"
            elif inside and line.strip().startswith("Path: "):
                paths.add(line.strip().removeprefix("Path: "))
        return paths

    def marked(m):
        # oomd's own word for each kill it makes.
        return m.execute("journalctl -o cat -u systemd-oomd | grep -F 'for killing'")[1]

    start_all()
    cgroup = {}
    for m in (guarded, control):
        m.wait_for_unit("user@1001.service")
        m.wait_until_succeeds(f"test -s {DIR}/thrash.pid && test -s {DIR}/sibling.pid", timeout=120)
        cgroup[m] = show(m, "ControlGroup")["ControlGroup"]
        for s in ("thrash", "sibling"):
            m.succeed(f"grep -qx '0::{cgroup[m]}' /proc/{pid(m, s)}/cgroup")
    # The server's cgroup and every one above it: a pressure watch on any of
    # them could pick the server.
    parts = cgroup[guarded].split("/")
    above = {"/".join(parts[:i]) or "/" for i in range(1, len(parts) + 1)}

    with subtest("no pressure watch sits above a server, and the server opts out of oomd"):
        pressure = monitored(guarded, "Memory Pressure Monitored CGroups")
        assert "/system.slice" in pressure and not pressure & above, pressure
        assert show(guarded, "ManagedOOMPreference") == {"ManagedOOMPreference": "omit"}
        guarded.succeed(f"getfattr --only-values -n user.oomd_omit /sys/fs/cgroup{cgroup[guarded]} | grep -qx 1")
        assert monitored(control, "Memory Pressure Monitored CGroups") & above

    state = ("MainPID", "InvocationID", "NRestarts", "ActiveState")
    before = {m: show(m, *state) for m in (guarded, control)}
    sibling = {m: pid(m, "sibling") for m in (guarded, control)}
    srv = server(guarded)
    for m in (guarded, control):
        m.succeed(f"echo >{DIR}/go")
    retry(lambda _: full(guarded, cgroup[guarded]) > 80, timeout_seconds=120)

    with subtest("the control: oomd kills the whole server, every session on it"):
        control.wait_until_succeeds(f"journalctl -o cat -u systemd-oomd | grep -F 'Marked {cgroup[control]} for killing'", timeout=120)
        control.wait_until_fails(f"kill -0 {sibling[control]}", timeout=30)
        control.wait_until_succeeds(f"[ $(systemctl --user -M dev@ show -p NRestarts --value {UNIT}) -gt {before[control]['NRestarts']} ]", timeout=60)

    with subtest("guarded, a minute of the same pressure leaves oomd nothing to kill"):
        # The control has been through the same by now: past oomd's 30s.
        end = time.monotonic() + 60
        while time.monotonic() < end:
            assert alive(guarded, sibling[guarded]) and alive(guarded, srv)
            assert marked(guarded) == "", marked(guarded)
            time.sleep(5)
        assert full(guarded, cgroup[guarded]) > 80, full(guarded, cgroup[guarded])

    with subtest("the session goes over the server's cap, and the kernel kills it alone"):
        guarded.succeed(f"systemctl --user -M dev@ set-property --runtime {UNIT} MemoryHigh=infinity")
        guarded.wait_until_succeeds(f"grep -qx 137 {DIR}/thrash.exit", timeout=120)
        guarded.succeed(f"grep -q '^oom_kill [1-9]' /sys/fs/cgroup{cgroup[guarded]}/memory.events")
        assert alive(guarded, sibling[guarded]) and alive(guarded, srv)
        assert show(guarded, *state) == before[guarded], show(guarded, *state)
        assert marked(guarded) == "", marked(guarded)
  '';
}
