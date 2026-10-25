# The agent sandboxes, for real: a host VM (a user, a pool laid out like the
# defaults, the agents and proj modules) boots sandboxes inside itself (nested
# KVM) and checks what they promise. Run it on a machine with /dev/kvm:
#
#   nix build .#checks.x86_64-linux.agents -L
{ self, pkgs }:
pkgs.testers.runNixOSTest {
  name = "agents";

  nodes.host = { lib, pkgs, ... }: let
    # The test commits, on the host and in the sandboxes.
    identity.programs.git.config.user = {
      name = "Test";
      email = "test@example.com";
    };
  in {
    imports = [ self.nixosModules.default identity ];
    burrow = {
      claude.enable = true;
      proj.enable = true;
      agents.enable = true;
      toolbox.enable = true;
      # Small sandboxes, so two fit (and not 2048: QEMU's microvm machine hangs).
      agents.memory = 1536;
      agents.vcpus = 2;
      # Everything the sandboxes log lands in this test's output.
      agents.guestModules = [ { burrow.agentGuest.consoleLevel = "info"; } identity ];
    };

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
      extraGroups = [ "wheel" "kvm" ];
    };
    users.groups.dev.gid = 1001;
    security.sudo.wheelNeedsPassword = false;

    virtualisation = {
      memorySize = 6144;
      cores = 4;
      emptyDiskImages = [ 16384 ];
    };

    boot.supportedFilesystems = [ "zfs" ];
    boot.zfs.forceImportRoot = false;
    networking.hostId = "8425e349";

    # rpool on the spare disk, with the default data dataset.
    systemd.services.make-rpool = {
      wantedBy = [ "multi-user.target" ];
      before = [ "agents-dataset.service" ];
      requiredBy = [ "agents-dataset.service" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      path = [ pkgs.zfs pkgs.coreutils ];
      script = ''
        zpool create -f -O mountpoint=none -O compression=zstd rpool /dev/vdb
        zfs create -o mountpoint=none rpool/safe
        zfs create -o mountpoint=/data rpool/safe/data
        mkdir -p /data/dev/src /data/lab
        chown -R dev:dev /data/dev /data/lab
      '';
    };
  };

  testScript = ''
    import json, time

    host.wait_for_unit("multi-user.target")
    host.wait_for_unit("agents-dataset.service")
    # proj alone runs burrow-events with no host or secret: kicks and
    # wake-ups only.
    host.wait_for_unit("burrow-events.service")
    host.succeed("mountpoint /agents")
    for unit in ["agent-proxy.service", "agent-proxy-open.service"]:
        host.wait_for_unit(unit)
    assert "agent-gc" in host.succeed("systemctl list-timers --all")

    def dev(cmd, check=True):
        f = host.succeed if check else host.execute
        return f(f"su - dev -c {json.dumps(cmd)}")

    def vm(agent, cmd):
        # One command in the sandbox, quoted for agent sh.
        return dev(f"agent sh {agent} bash -c {json.dumps(cmd)} </dev/null")

    dev("cd /data/dev/src && mkdir demo && cd demo && git init -q -b main && echo one >a && git add a && git commit -qm one")
    dev("echo lab >/data/lab/disk.img")

    with subtest("one command gives a running sandbox"):
        t = time.time()
        status, out = host.execute("su - dev -c 'agent new demo --with /data/lab' 2>&1")
        if status != 0:
            print(out)
            print(host.execute("ls -lanR /agents/demo-1/home; journalctl --no-pager -u microvm@demo-1 -u agent-shares@demo-1 | tail -n 300")[1])
            raise Exception("agent new failed")
        a = out.strip().splitlines()[-1]
        print(f"agent new took {time.time() - t:.1f}s (nested, inside a test VM)")
        assert a == "demo-1", a
        assert "running" in dev("agent ls")
        assert vm(a, "hostname").strip() == a

    with subtest("it sees its project and --with paths, and nothing else of /data"):
        assert vm(a, "pwd").strip() == "/data/dev/src/demo"
        assert vm(a, "cat a").strip() == "one"
        assert vm(a, "cat /data/lab/disk.img").strip() == "lab"
        assert vm(a, "ls /data/dev/src").split() == ["demo"]

    with subtest("its writes are dev's on the host, root's too, and stay in the clone"):
        vm(a, "echo vm >b && sudo touch c && sudo mkdir d && echo changed >/data/lab/disk.img")
        clone = dev(f"agent path {a}").strip()
        assert clone == "/agents/demo-1/data/dev/src/demo", clone
        for f in ["b", "c", "d"]:
            assert dev(f"stat -c %U:%G {clone}/{f}").strip() == "dev:dev", f
        host.fail("test -e /data/dev/src/demo/b")
        assert dev("cat /data/lab/disk.img").strip() == "lab"

    with subtest("Claude is there and never asks"):
        vm(a, "claude --version")
        settings = json.loads(vm(a, "cat /etc/claude-code/managed-settings.json"))
        assert settings["permissions"]["defaultMode"] == "bypassPermissions"
        assert settings["skipDangerousModePermissionPrompt"] is True
        assert json.loads(vm(a, "cat ~/.claude.json"))["projects"]["/data/dev/src/demo"]["hasTrustDialogAccepted"]

    with subtest("it has KVM of its own, for projects with VMs"):
        vm(a, "test -c /dev/kvm && test -w /dev/kvm")

    with subtest("egress: the allowlist through the proxy, and nothing else"):
        def connect(url, extra=""):
            return vm(a, f"curl -s -o /dev/null --max-time 10 -w '%{{http_connect}}' {extra} {url} || true").strip()
        # Refused by the proxy itself.
        assert connect("https://example.com/") == "403", connect("https://example.com/")
        # Allowed: the proxy tries (and, with no internet in a test, fails
        # upstream rather than refusing).
        assert connect("https://github.com/") not in ("403", "000"), connect("https://github.com/")
        # Around the proxy: no route out, and no way to the host's own ports.
        vm(a, "! curl -s --noproxy '*' --max-time 5 http://1.1.1.1/")
        vm(a, "! curl -s --noproxy '*' --max-time 5 http://10.0.2.2:3129/")
        vm(a, "! getent hosts github.com")

    with subtest("--open lifts the allowlist"):
        o = dev("agent new demo --open").strip()
        assert vm(o, "curl -s -o /dev/null --max-time 10 -w '%{http_connect}' https://example.com/ || true").strip() != "403"

    with subtest("a second sandbox costs only its own changes"):
        used = int(host.succeed(f"zfs get -Hp -o value used rpool/agents/{o}"))
        print(f"{o} uses {used} bytes")
        assert used < 256 * 1024 * 1024, used

    with subtest("killing a VM changes nothing outside it"):
        host.succeed(f"systemctl kill -s KILL microvm@{a}.service")
        host.succeed("test \"$(cat /data/dev/src/demo/a)\" = one")
        host.fail("test -e /data/dev/src/demo/b")
        host.succeed(f"test -e {clone}/b")
        # And it comes back by itself.
        host.wait_until_succeeds(f"su - dev -c 'agent sh {a} true </dev/null'", timeout=120)

    with subtest("fetch brings its work back as refs, without running its git config"):
        vm(a, "git checkout -qb feat && echo two >a && git commit -qam two && echo wip >e")
        # A sandbox can plant anything in its repo's config; the host must not run it.
        vm(a, "git config core.fsmonitor 'touch /tmp/pwned-fsmonitor' && git config filter.x.clean 'touch /tmp/pwned-filter' && echo '* filter=x' >.gitattributes")
        out = dev(f"agent fetch {a}")
        print(out)
        repo = "git -C /data/dev/src/demo"
        assert dev(f"{repo} show refs/agents/{a}/heads/feat:a").strip() == "two"
        assert dev(f"{repo} show refs/agents/{a}/worktree:e").strip() == "wip"
        assert dev(f"{repo} show refs/agents/{a}/worktree:b").strip() == "vm"
        host.fail("test -e /tmp/pwned-fsmonitor")
        host.fail("test -e /tmp/pwned-filter")
        # The real project's branches and files didn't move.
        assert dev(f"{repo} rev-parse --abbrev-ref HEAD").strip() == "main"
        host.fail("test -e /data/dev/src/demo/e")

    with subtest("proj runs a job inside a sandbox"):
        job = dev("proj run --vm demo 'say hi'").strip()
        sandbox = host.succeed(f"cat /data/dev/jobs/{job}/agent").strip()
        assert sandbox == "demo-3", sandbox
        assert "[vm demo-3]" in dev("proj status")
        # No Claude login in a test, so the job ends without an answer; what
        # matters is that it ran claude inside the sandbox and kept its output.
        host.wait_until_succeeds(f"[ \"$(su - dev -c 'proj status' | grep {job} | awk '{{print $2}}')\" != running ]", timeout=180)
        host.succeed(f"test -s /data/dev/jobs/{job}/1.jsonl -o -s /data/dev/jobs/{job}/1.outcome")

    with subtest("rm stops and saves, keep undoes it, gc deletes once expired"):
        dev(f"agent rm {o}")
        assert "removed" in dev("agent ls")
        dev(f"agent keep {o}")
        assert "removed" not in dev("agent ls")
        dev(f"agent rm {o}")
        host.succeed(f"touch -d '2 days ago' /agents/{o}/removed")
        host.succeed("systemctl start agent-gc.service")
        host.fail(f"zfs list rpool/agents/{o}")
        host.fail(f"zfs list rpool/safe/data@agent-{o}")
        host.fail(f"test -e /agents/{o}")

    with subtest("gc stops a sandbox unused for a day, and saves and deletes it after a week"):
        host.succeed(f"touch -d '2 days ago' /agents/{a}/last-used")
        host.succeed("systemctl start agent-gc.service")
        host.fail(f"systemctl is-active microvm@{a}.service")
        host.succeed(f"zfs list rpool/agents/{a}")
        host.succeed(f"touch -d '8 days ago' /agents/{a}/last-used")
        host.succeed("systemctl start agent-gc.service")
        host.fail(f"zfs list rpool/agents/{a}")
        # Its work outlives it.
        assert dev(f"git -C /data/dev/src/demo show refs/agents/{a}/heads/feat:a").strip() == "two"

    with subtest("a stopped sandbox comes back on use"):
        dev("agent stop demo-3")
        assert "stopped" in dev("agent ls")
        assert vm("demo-3", "hostname").strip() == "demo-3"
  '';
}
