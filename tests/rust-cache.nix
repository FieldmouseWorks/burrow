# rust-cache, for real: a machine with the cache on for dev and for a CI
# runner's user, ci. A crate with no dependencies, built without incremental
# compilation, stands in for a build's dependencies. The checks:
# - a fresh target dir's build comes out of the cache, wherever the target
#   dir is and whether CARGO_TARGET_DIR names it, for dev and for the runner,
#   whose sandbox makes the rest of the system read-only, and for a unit
#   systemd-run started;
# - all of a user's units share one daemon: a runner that has been building
#   all along takes what another unit just stored (a daemon of its own would
#   still miss it), and the compile runs in the runner's unit, not the
#   daemon's;
# - the daemon runs nothing, even for a build that skips the wrapper and asks
#   it to compile a program it can reach, and it sees nothing but sccache and
#   the cache;
# - a crate bigger than sccache's default message size is cached too;
# - the C a build script compiles with the cc crate comes out of the cache
#   too, which cc does only for a RUSTC_WRAPPER named sccache;
# - a cached `cargo check` never stands in for clippy, and a compile error
#   still fails the build;
# - whatever keeps the cache from working compiles without it: incremental
#   builds sccache won't take, the user's own SCCACHE_ settings, the daemon
#   stopped, and a socket with no daemon behind it, which mustn't start a
#   stray server either;
# - nobody else reaches a user's cache, and the daemon listens on no port;
# - a runner that outlives a switch builds with the new system's wrapper, and
#   still builds after a switch that turns the cache off.
#
#   nix build .#checks.x86_64-linux.rust-cache -L
{ self, pkgs, inputs }:
let
  crate = name: body: pkgs.runCommand "crate-${name}" { } ''
    mkdir -p $out/src
    cat >$out/Cargo.toml <<'EOF'
    [package]
    name = "${name}"
    version = "0.1.0"
    edition = "2021"
    EOF
    echo ${pkgs.lib.escapeShellArg body} >$out/src/lib.rs
  '';
  # clippy's needless_return, which rustc doesn't mind.
  ok = crate "ok" "pub fn next(x: i32) -> i32 { return x + 1; }";
  broken = crate "broken" ''pub fn next(x: i32) -> i32 { "not a number" }'';

  # A crate whose build script compiles C with the cc crate, set up to build
  # offline from cc and its dependencies, vendored.
  vendored = pkgs.rustPlatform.importCargoLock { lockFile = ./rust-cache/c-build/Cargo.lock; };
  cBuild = pkgs.runCommand "crate-c-build" { } ''
    cp -r ${./rust-cache/c-build} $out
    chmod -R u+w $out
    mkdir $out/.cargo
    cat >$out/.cargo/config.toml <<'EOF'
    [source.crates-io]
    replace-with = "vendored"
    [source.vendored]
    directory = "${vendored}"
    EOF
  '';

  # A program a build plants where the daemon can reach it, noting which
  # cgroup it runs in. Linked against the glibc of the daemon's own sccache,
  # so it would run in the daemon's chroot too.
  planted = inputs.nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system}.writeCBin "planted" ''
    #include <stdio.h>
    int main(void) {
      FILE *in = fopen("/proc/self/cgroup", "r"), *out = fopen("/dev/shm/planted.log", "a");
      int c;
      if (!in || !out) return 1;
      while ((c = fgetc(in)) != EOF) fputc(c, out);
      return 0;
    }
  '';

  # rustc, noting which cgroup each compile runs in.
  spy = pkgs.writeShellScript "rustc-spy" ''
    cat /proc/self/cgroup >>spy.log
    exec ${pkgs.rustc}/bin/rustc "$@"
  '';

  # A CI runner, as far as the cache can tell: one long-lived unit, sandboxed
  # as remi's are, running each job it's given. A job is a script in
  # queue/; its output and exit status land next to it.
  runner = pkgs.writeShellScript "runner" ''
    mkdir -p queue
    while :; do
      for job in queue/*.sh; do
        [ -e "$job" ] || continue
        status=0
        bash "$job" >"''${job%.sh}.log" 2>&1 || status=$?
        echo "$status" >"''${job%.sh}.status"
        rm "$job"
      done
      sleep 0.2
    done
  '';
in
pkgs.testers.runNixOSTest {
  name = "rust-cache";

  nodes.machine = {
    imports = [ self.nixosModules.rust-cache ];
    burrow.rustCache = {
      enable = true;
      users = [ "ci" ];
    };
    environment.systemPackages = with pkgs; [ cargo rustc clippy gcc python3 ];

    users.users.dev = {
      isNormalUser = true;
      uid = 1001;
      group = "dev";
    };
    users.groups.dev.gid = 1001;
    # Has no cache.
    users.users.other = {
      isNormalUser = true;
      uid = 1002;
      group = "other";
    };
    users.groups.other.gid = 1002;
    users.users.ci = {
      isSystemUser = true;
      group = "ci";
      home = "/var/lib/ci";
    };
    users.groups.ci = { };

    systemd.services.runner = {
      wantedBy = [ "multi-user.target" ];
      path = with pkgs; [ bash coreutils cargo rustc ];
      serviceConfig = {
        User = "ci";
        StateDirectory = "ci";
        WorkingDirectory = "/var/lib/ci";
        ExecStart = runner;
        NoNewPrivileges = true;
        ProtectHome = true;
        ProtectSystem = "strict";
        PrivateTmp = true;
      };
    };
    virtualisation.memorySize = 2048;
    # The same with the cache moved, which changes the wrapper, and with it
    # off. Neither changes the runner's unit, so a switch leaves it running.
    specialisation.moved.configuration.burrow.rustCache.dir = "/var/cache/sccache-moved";
    specialisation.off.configuration.burrow.rustCache.enable = pkgs.lib.mkForce false;
  };

  testScript = { nodes, ... }: ''
    import json
    import shlex

    sccache = "${nodes.machine.systemd.services.rust-cache-dev.serviceConfig.ExecStart}"
    build = "CARGO_INCREMENTAL=0 cargo build -q"

    def place(src, path, owner):
        machine.succeed(f"cp -rT {src} {path} && chmod -R u+w {path} && chown -R {owner}: {path}")

    def run(cmd, user="dev"):
        return machine.execute(f"su - {user} -c {shlex.quote(cmd)} 2>&1")

    jobs = 0

    def job(cmd):
        global jobs
        jobs += 1
        q = f"/var/lib/ci/queue/{jobs}"
        machine.succeed(f"cat >{q}.tmp <<'EOF'\n{cmd}\nEOF")
        machine.succeed(f"chown ci: {q}.tmp && mv {q}.tmp {q}.sh")
        machine.wait_until_succeeds(f"test -e {q}.status")
        return int(machine.succeed(f"cat {q}.status")), machine.succeed(f"cat {q}.log")

    def oneshot(cmd):
        return machine.execute(
            "systemd-run --uid=ci --gid=ci --wait --pipe --collect"
            " -p StateDirectory=ci -p WorkingDirectory=/var/lib/ci"
            " -p ProtectSystem=strict -p ProtectHome=yes -p PrivateTmp=yes"
            " -p Environment=PATH=/run/current-system/sw/bin"
            f" bash -c {shlex.quote(cmd)} 2>&1"
        )

    def stats(user, root="/var/cache/sccache"):
        out = machine.succeed(
            f"runuser -u {user} -- env SCCACHE_SERVER_UDS={root}/{user}/sccache.sock"
            f" {sccache} --show-stats --stats-format json"
        )
        return json.loads(out)["stats"]

    def hits(user, root="/var/cache/sccache", language="Rust"):
        return stats(user, root)["cache_hits"]["counts"].get(language, 0)

    def ok(result, what):
        status, out = result
        assert status == 0, f"{what} failed: {out}"
        return out

    machine.wait_for_unit("rust-cache-dev.service")
    machine.wait_for_unit("rust-cache-ci.service")
    machine.wait_for_unit("runner.service")

    with subtest("each user's daemon is up, on a socket only its user can reach"):
        for user in ["dev", "ci"]:
            d = f"/var/cache/sccache/{user}"
            assert machine.succeed(f"stat -c '%U %a' {d}").split() == ["root", "711"]
            run_dir = f"/run/rust-cache-{user}"
            for sub in [f"{d}/cache", run_dir]:
                assert machine.succeed(f"stat -c '%U %a' {sub}").split() == [user, "700"]
            assert machine.succeed(f"readlink {d}/sccache.sock").strip() == f"{run_dir}/sccache.sock"
            machine.wait_until_succeeds(f"test -S {d}/sccache.sock")
        machine.fail("systemctl cat rust-cache-other.service")
        machine.fail("ss -Hltnp | grep sccache")
        status, out = run(
            "python3 -c 'import socket; socket.socket(socket.AF_UNIX).connect(\"/var/cache/sccache/dev/sccache.sock\")'",
            "other",
        )
        assert status != 0 and "PermissionError" in out, f"other reached dev's daemon: {out}"

    with subtest("a fresh target dir's build comes out of the cache"):
        place("${ok}", "/home/dev/a", "dev")
        ok(run(f"cd a && {build}"), "dev's first build")
        before = hits("dev")
        ok(run(f"cd a && rm -rf target && {build}"), "dev's second build")
        assert hits("dev") > before, "dev's fresh target dir missed the cache"
        # However the target dir is named, and wherever it is.
        for var, dir in [("CARGO_TARGET_DIR", "t1"), ("CARGO_TARGET_DIR", "t2"), ("CARGO_BUILD_TARGET_DIR", "t3")]:
            before = hits("dev")
            ok(run(f"cd a && {var}=/home/dev/{dir} {build}"), f"a build into {var}={dir}")
            assert hits("dev") > before, f"a fresh target dir named by {var} missed the cache"

    with subtest("a crate bigger than sccache's default message size is cached too"):
        place("${ok}", "/home/dev/big", "dev")
        ok(
            run(
                "cd big && head -c 12000000 /dev/urandom >src/blob.bin"
                " && echo 'pub static BLOB: &[u8] = include_bytes!(\"blob.bin\");' >>src/lib.rs"
            ),
            "making the crate big",
        )
        ok(run(f"cd big && {build}"), "the big crate's first build")
        before = hits("dev")
        ok(run(f"cd big && rm -rf target && {build}"), "the big crate's second build")
        assert hits("dev") > before, "the big crate missed the cache"

    with subtest("a build script's C compiles come out of the cache too"):
        place("${cBuild}", "/home/dev/c", "dev")
        ok(run(f"cd c && {build} --offline"), "the C crate's first build")
        before = hits("dev", language="C/C++")
        ok(run(f"cd c && rm -rf target && {build} --offline"), "the C crate's second build")
        assert hits("dev", language="C/C++") > before, "a build script's C compile missed the cache"

    with subtest("all of a user's units share one daemon, and compile in their own"):
        # The runner builds first, so anything of its own that serves it is
        # already running when another unit stores the crate it builds next.
        place("${ok}", "/var/lib/ci/a", "ci")
        status, out = job(f"cd a && {build}")
        assert status == 0, out
        place("${ok}", "/var/lib/ci/x", "ci")
        ok(oneshot(f"cd x && {build}"), "the oneshot unit's build")
        before = hits("ci")
        status, out = job(f"cd x && rm -rf target && {build}")
        assert status == 0, out
        assert hits("ci") > before, "the runner missed what the other unit stored"
        # A compiler of its own, so a miss: this one compiles.
        status, out = job(f"cd x && rm -rf target && RUSTC=${spy} {build}")
        assert status == 0, out
        cgroups = machine.succeed("cat /var/lib/ci/x/spy.log")
        assert "runner.service" in cgroups and "rust-cache" not in cgroups, cgroups
        assert machine.succeed("pgrep -c -u ci sccache").strip() == "1"

    with subtest("the daemon runs nothing a build names, and sees only its cache"):
        # A build that skips the wrapper, so sccache asks the daemon to compile
        # (and first to tell what the compiler is), with a compiler in
        # /dev/shm, which the daemon shares with the host, from /, which it
        # has too. The daemon refuses, and sccache compiles in the build.
        before = stats("ci")["compile_requests"]
        status, out = job(
            "install -m 755 ${planted}/bin/planted /dev/shm/rustc"
            " && cd / && SCCACHE_SERVER_UDS=/var/cache/sccache/ci/sccache.sock"
            f" timeout 120 {sccache} /dev/shm/rustc -vV"
        )
        ran = machine.succeed("cat /dev/shm/planted.log")
        assert "rust-cache" not in ran, f"the daemon ran a build's compiler: {ran}"
        assert stats("ci")["compile_requests"] > before, "the build never asked the daemon to compile"
        assert status == 0 and "runner.service" in ran, f"the build didn't compile itself: {out} {ran}"
        # A key can name any path, so the daemon's root holds nothing else.
        pid = machine.succeed("systemctl show -P MainPID rust-cache-ci.service").strip()
        for path in ["/run/current-system", "/var/lib/ci", "/home", "/bin/sh", "${pkgs.rustc}"]:
            machine.fail(f"test -e /proc/{pid}/root{path}")

    with subtest("a cached check never stands in for clippy, and a compile error still fails"):
        place("${ok}", "/home/dev/lint", "dev")
        ok(run("cd lint && CARGO_INCREMENTAL=0 cargo check -q"), "cargo check")
        status, out = run("cd lint && rm -rf target && CARGO_INCREMENTAL=0 cargo clippy -q -- -D warnings")
        assert status != 0 and "needless_return" in out, f"clippy let the lint through: {out}"
        place("${broken}", "/home/dev/broken", "dev")
        for _ in range(2):
            status, out = run(f"cd broken && {build}")
            assert status != 0 and "E0308" in out, f"a broken crate built: {out}"

    with subtest("whatever keeps the cache from working compiles without it"):
        ok(run("cd a && rm -rf target && CARGO_INCREMENTAL=1 cargo build -q"), "CARGO_INCREMENTAL=1")
        ok(run("cd a && rm -rf target && CARGO_BUILD_INCREMENTAL=true cargo build -q"), "build.incremental")
        # cargo itself takes only true and false there; sccache refuses a
        # compile that has a 1 anyway.
        ok(
            run("cd a && CARGO_BUILD_INCREMENTAL=1 $RUSTC_WRAPPER rustc --crate-type lib --edition 2021 src/lib.rs --out-dir out"),
            "CARGO_BUILD_INCREMENTAL=1",
        )
        before = hits("dev")
        ok(run(f"cd a && rm -rf target && SCCACHE_LOG=debug {build}"), "SCCACHE_LOG")
        assert hits("dev") > before, "the user's SCCACHE_LOG kept the build out of the cache"

        machine.succeed("systemctl stop rust-cache-dev.service")
        machine.fail("test -e /run/rust-cache-dev")
        out = ok(run(f"cd a && rm -rf target && {build}"), "a build with the daemon stopped")
        assert "sccache" not in out, out

        # A socket with nobody behind it, as a daemon that died before systemd
        # cleared up after it leaves one.
        machine.succeed("install -d -o dev -g dev -m 700 /run/rust-cache-dev")
        ok(
            run("python3 -c 'import socket; socket.socket(socket.AF_UNIX).bind(\"/run/rust-cache-dev/sccache.sock\")'"),
            "leaving a socket",
        )
        machine.succeed("test -S /var/cache/sccache/dev/sccache.sock")
        ok(run(f"cd a && rm -rf target && {build}"), "a build with a dead daemon's socket")
        machine.fail("pgrep -u dev sccache")
        machine.fail("test -e /home/dev/.cache/sccache")

        machine.succeed("systemctl start rust-cache-dev.service")
        machine.wait_until_succeeds("test -S /var/cache/sccache/dev/sccache.sock")
        before = hits("dev")
        ok(run(f"cd a && rm -rf target && {build}"), "a build with the daemon back")
        assert hits("dev") > before, "the daemon came back without its cache"

        place("${ok}", "/home/other/a", "other")
        before = hits("dev")
        ok(run(f"cd a && {build}", "other"), "a build with no cache")
        ok(run(f"cd a && rm -rf target && USER=dev {build}", "other"), "a build naming someone else's cache")
        assert hits("dev") == before, "other's build used dev's cache"

    with subtest("a runner that outlives a switch builds with the new system's wrapper"):
        base = "${nodes.machine.system.build.toplevel}"
        pid = machine.succeed("systemctl show -P MainPID runner.service").strip()
        machine.succeed(f"{base}/specialisation/moved/bin/switch-to-configuration test")
        assert machine.succeed("systemctl show -P MainPID runner.service").strip() == pid, "the switch restarted the runner"
        # With the old cache cleared away, only the new wrapper finds a daemon.
        machine.succeed("rm -rf /var/cache/sccache")
        machine.wait_until_succeeds("test -S /var/cache/sccache-moved/ci/sccache.sock")
        status, out = job(f"cd x && rm -rf target && {build} && rm -rf target && {build}")
        assert status == 0, out
        assert hits("ci", "/var/cache/sccache-moved") > 0, "the runner built with the wrapper it started with"

    with subtest("a runner that outlives a switch turning the cache off still builds"):
        machine.succeed(f"{base}/specialisation/off/bin/switch-to-configuration test")
        assert machine.succeed("systemctl show -P MainPID runner.service").strip() == pid, "the switch restarted the runner"
        machine.fail("systemctl is-active rust-cache-ci.service")
        status, out = job(f"cd x && rm -rf target && {build}")
        assert status == 0, f"the runner's build failed with the cache off: {out}"
  '';
}
