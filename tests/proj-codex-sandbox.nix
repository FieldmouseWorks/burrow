# Exercise the pinned CLI's actual filesystem policy, without an API call:
# worktree commits need an explicit grant to the clone's git metadata. Then
# a proj job on Codex, for real (the `jobs` machine): proj's own argv to the
# pinned CLI, with a stand-in model that asks for what a job does, and what
# the job's sandbox let through, and a person's job, with no App of its own,
# acting as the machine's App to push its work. Then
# its hooks, with a stand-in model that asks for one command: the example
# machine's /etc/codex/requirements.toml runs the guard on every command a
# Codex job (as the machine's user) runs, and refuses what has no undo here
# (no ZFS in this VM, so no snapshot) or would hide a commit from the job's
# hooks, and a patch by Codex's own apply_patch that deletes a file; another
# user's Codex runs the same command and patch unguarded.
{ self, pkgs }:
let
  example = self.nixosConfigurations.example.config;
  model = "${self.packages.${pkgs.stdenv.hostPlatform.system}.codex-stand-in}/bin/codex-stand-in";
  modelConfig = pkgs.writeText "codex-model.toml" ''
    model = "stand-in"
    model_provider = "stand-in"
    approval_policy = "never"
    [model_providers.stand-in]
    name = "stand-in"
    base_url = "http://127.0.0.1:18555/v1"
    wire_api = "responses"
  '';
  agents = import ../modules/autoland/agents/package.nix { inherit pkgs; };
  hook = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj-git-hook";
  hooks = pkgs.runCommand "codex-test-hooks" { } ''
    mkdir -p $out
    ln -s ${hook} $out/commit-msg
    ln -s ${hook} $out/pre-push
  '';
  settings = git: pkgs.writeText "codex-sandbox-test.toml" ''
    sandbox_mode = "workspace-write"
    approval_policy = "never"
    default_permissions = "proj"
    [permissions.proj]
    filesystem = { ":root" = "read", "/tmp" = "write", "/work/project" = "read", "/work/scratch" = "write"${pkgs.lib.optionalString git ", \"/work/project/.git\" = \"write\""} }
    network.enabled = true
  '';
  inherit (import ./proj/stand-ins.nix { inherit pkgs; }) claude;
  # What a job does, each command leaving a mark in the job's own dir that
  # only its success writes, so a sandbox that refuses one shows: fetch,
  # make a worktree in its scratch and commit there, reach its App's token
  # socket, write its job's state (`proj done`), and, to be refused, write
  # outside its roots, read Codex's login where the login shell moved it
  # (through its symlink, and at its target in the machine's /tmp, which
  # Codex's own start keeps, as it does git's global config there: commits
  # read it, nothing writes it), change Codex's config, and reach what runs things
  # outside the sandbox: the user's D-Bus and the systemd manager's private
  # socket, spoken to directly, and the user's tmux servers: one at a path of
  # its own (-S), one bound by a relative name, one under a TMUX_TMPDIR the
  # job's env doesn't have, and two started mid-command, in tmux's default
  # dir and that custom one; a server of the user's in a network namespace
  # of its own, as the compile cache's is, which the kernel's socket table
  # here doesn't list; and a Codex daemon's socket, in the dir the CLI masks
  # itself. The job's /tmp is its own, its user's keys out of sight, and
  # nix goes through the daemon. And it deletes a file of the project's,
  # which the guard lets through only once its undo snapshot is taken: the
  # project is on a ZFS dataset the guard snapshots, from outside the user
  # namespace Codex runs in.
  connect = path: "python3 -c 'import socket; socket.socket(socket.AF_UNIX).connect(\"${path}\")'";
  calls = cmds: map (cmd: [ "exec_command" { inherit cmd; } ]) cmds;
  firstCalls = calls [
    "python3 -c 'import os; os.listdir(\"/var/lib/burrow/proj-questions\")' || touch /data/dev/jobs/$PROJ_JOB/control-read-refused"
    "python3 -c 'open(\"/var/lib/burrow/proj-questions/forged\", \"w\").close()' || touch /data/dev/jobs/$PROJ_JOB/control-write-refused"
    "proj probe deployed > /data/dev/jobs/$PROJ_JOB/host-probe && touch /data/dev/jobs/$PROJ_JOB/host-probed"
    "git fetch -q origin && touch /data/dev/jobs/$PROJ_JOB/fetched"
    "git worktree add -q -b work \"$PROJ_SCRATCH/tree\" origin/trunk && git -C \"$PROJ_SCRATCH/tree\" commit -q --allow-empty -m changed && touch /data/dev/jobs/$PROJ_JOB/committed"
    "python3 -c 'import os, socket; socket.socket(socket.AF_UNIX).connect(os.environ[\"GH_TOKEN_SOCKET\"])' && touch /data/dev/jobs/$PROJ_JOB/connected"
    "grep -qx 'NoNewPrivs:[[:space:]]1' /proc/self/status && touch /data/dev/jobs/$PROJ_JOB/no-new-privs"
    "touch /data/dev/elsewhere/written || touch /data/dev/jobs/$PROJ_JOB/elsewhere-refused"
    "cat \"$CODEX_HOME/auth.json\" || touch /data/dev/jobs/$PROJ_JOB/login-refused"
    "cat /tmp/codex-home/auth.json || touch /data/dev/jobs/$PROJ_JOB/login-target-refused"
    "touch \"$CODEX_HOME/config.toml\" || touch /data/dev/jobs/$PROJ_JOB/codex-config-refused"
    "test \"$(git config --global --show-origin user.email)\" = \"$(printf 'file:/tmp/dev-gitconfig\\tdev@example.test')\" && ! touch /tmp/dev-gitconfig && touch /data/dev/jobs/$PROJ_JOB/gitconfig-read-only"
    "${connect "/run/user/1001/bus"} || touch /data/dev/jobs/$PROJ_JOB/bus-refused"
    "${connect "/run/user/1001/systemd/private"} || touch /data/dev/jobs/$PROJ_JOB/manager-refused"
    "tmux -S /tmp/served.sock new-window 'touch /data/dev/elsewhere/by-socket' || touch /data/dev/jobs/$PROJ_JOB/tmux-socket-refused"
    "TMUX_TMPDIR=/data/dev/tmux-custom tmux -L custom new-window 'touch /data/dev/elsewhere/by-custom' || touch /data/dev/jobs/$PROJ_JOB/tmux-custom-refused"
    "tmux -S /tmp/served-rel.sock new-window 'touch /data/dev/elsewhere/by-relative' || touch /data/dev/jobs/$PROJ_JOB/tmux-relative-refused"
    "${connect "/run/cache-stand-in/cache.sock"} || touch /data/dev/jobs/$PROJ_JOB/netns-refused"
    "${connect "/tmp/codex-daemon-1001/app.sock"} || touch /data/dev/jobs/$PROJ_JOB/daemon-refused"
    "cat ~/.ssh/id_test || touch /data/dev/jobs/$PROJ_JOB/keys-refused"
    "touch /tmp/job-tmp && test \"$TMPDIR\" = /tmp && touch /data/dev/jobs/$PROJ_JOB/tmp-own"
    "test \"$NIX_REMOTE\" = daemon && nix-store -q --hash \"$(readlink -f \"$(command -v bash)\")\" && touch /data/dev/jobs/$PROJ_JOB/store-queried"
    "rm victim && touch /data/dev/jobs/$PROJ_JOB/removed"
  ] ++ [
    # Servers the user starts once this command runs, in tmux's default dir
    # (not there when the job started) and in the custom one.
    [ "exec_command" {
      cmd = "touch /data/dev/jobs/$PROJ_JOB/waiting && timeout 25 sh -c 'until test -e /data/dev/jobs/$PROJ_JOB/late-started; do sleep 0.2; done' && { tmux -L host new-window 'touch /data/dev/elsewhere/by-tmux' || touch /data/dev/jobs/$PROJ_JOB/tmux-refused; } && { TMUX_TMPDIR=/data/dev/tmux-custom tmux -L late new-window 'touch /data/dev/elsewhere/by-late' || touch /data/dev/jobs/$PROJ_JOB/tmux-late-refused; }";
      yield_time_ms = 30000;
    } ]
  ] ++ calls [
    "proj done"
  ];
  # A person's job, started as proj-inbox starts one for a person's issue: with
  # no App of its own. Its sandbox hides the person's own logins, so it acts
  # as the machine's App: that App's token socket and credential helper, not
  # the person's gh login or the GH_TOKEN of their profile. And it publishes
  # its work: a branch pushed to origin.
  personCalls = calls [
    "test -z \"$GH_TOKEN\" && test \"$GH_TOKEN_SOCKET\" = /run/gh-token/workers.sock && python3 -c 'import os, socket; socket.socket(socket.AF_UNIX).connect(os.environ[\"GH_TOKEN_SOCKET\"])' && touch /data/dev/jobs/$PROJ_JOB/person-connected"
    "git config --get-all credential.helper | tail -n 1 | grep -q 'gh-token git-credential' && touch /data/dev/jobs/$PROJ_JOB/person-helper"
    "cat ~/.config/gh/hosts.yml || touch /data/dev/jobs/$PROJ_JOB/person-login-refused"
    "git fetch -q origin && git worktree add -q -b person \"$PROJ_SCRATCH/person\" origin/trunk && git -C \"$PROJ_SCRATCH/person\" commit -q --allow-empty -m person && git -C \"$PROJ_SCRATCH/person\" push -q origin person && touch /data/dev/jobs/$PROJ_JOB/person-pushed"
    "proj done"
  ];
  # Its next turn, a resumed Codex with a fresh profile, held to the same.
  resumedCalls = calls [
    "touch /data/dev/elsewhere/resumed || touch /data/dev/jobs/$PROJ_JOB/resumed-elsewhere-refused"
    "cat \"$CODEX_HOME/auth.json\" || touch /data/dev/jobs/$PROJ_JOB/resumed-login-refused"
    "touch \"$CODEX_HOME/config.toml\" || touch /data/dev/jobs/$PROJ_JOB/resumed-codex-config-refused"
    # `proj done` took the last turn's scratch with it.
    "git worktree add -q --detach \"$PROJ_SCRATCH/again\" origin/trunk && git -C \"$PROJ_SCRATCH/again\" commit -q --allow-empty -m again && ! touch /tmp/dev-gitconfig && touch /data/dev/jobs/$PROJ_JOB/resumed-committed"
    "${connect "/run/user/1001/bus"} || touch /data/dev/jobs/$PROJ_JOB/resumed-bus-refused"
    "tmux -L host new-window 'touch /data/dev/elsewhere/resumed-by-tmux' || touch /data/dev/jobs/$PROJ_JOB/resumed-tmux-refused"
    "tmux -S /tmp/served-rel.sock new-window 'touch /data/dev/elsewhere/resumed-by-relative' || touch /data/dev/jobs/$PROJ_JOB/resumed-tmux-relative-refused"
    "${connect "/run/cache-stand-in/cache.sock"} || touch /data/dev/jobs/$PROJ_JOB/resumed-netns-refused"
    "touch /tmp/job-tmp && test \"$TMPDIR\" = /tmp && touch /data/dev/jobs/$PROJ_JOB/resumed-tmp-own"
    "rm victim-again && touch /data/dev/jobs/$PROJ_JOB/resumed-removed"
    "proj done"
  ];
  # A server that only listens: enough for a connect() to succeed.
  serve = pkgs.writeText "serve.py" ''
    import socket, sys, time
    s = socket.socket(socket.AF_UNIX)
    s.bind(sys.argv[1])
    s.listen()
    time.sleep(10**9)
  '';
  jobCalls = pkgs.writeText "codex-job-calls.json" (builtins.toJSON firstCalls);
  bothCalls = pkgs.writeText "codex-job-calls-resumed.json" (builtins.toJSON (firstCalls ++ resumedCalls));
  personJobCalls = pkgs.writeText "codex-person-calls.json" (builtins.toJSON personCalls);
  # Configs that would widen a profile proj names: the user's (with its
  # default selection, an inherited parent and an extra grant) and the
  # trusted project's. Neither may reach the job's sandbox.
  widening = pkgs.writeText "codex-widening.toml" ''
    [projects."/data/dev/src/web"]
    trust_level = "trusted"
    [permissions.wide]
    filesystem = { ":root" = "write" }
    network.enabled = true
    [permissions.proj]
    extends = "wide"
    filesystem = { "/data/dev/elsewhere" = "write" }
  '';
  projectWidening = pkgs.writeText "codex-project-widening.toml" ''
    [permissions.proj]
    filesystem = { "/data/dev/elsewhere" = "write" }
  '';
in
pkgs.testers.runNixOSTest {
  name = "proj-codex-sandbox";
  # proj, its Codex role and the workers App, the machine's own, as a
  # machine runs them, with the guard's undo snapshots on a ZFS pool of its
  # own.
  nodes.jobs = {
    imports = [ self.nixosModules.proj self.nixosModules.undo ];
    virtualisation.emptyDiskImages = [ 512 ];
    boot.supportedFilesystems = [ "zfs" ];
    boot.zfs.forceImportRoot = false;
    networking.hostId = "8425e349";
    burrow = {
      undo = {
        enable = true;
        datasets = [ "tank/web" ];
      };
      claude.package = claude;
      proj = {
        enable = true;
        minFreeMemory = 0;
      };
      github.apps.workers = {
        id = 7;
        slug = "sam-workers";
        keyFile = "/run/keys/workers.pem";
        users = [ "dev" ];
      };
      github.machinery = "workers";
    };
    # A server of the user's in a network namespace of its own, as the
    # compile cache's daemon is.
    systemd.services.cache-stand-in = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        User = "dev";
        RuntimeDirectory = "cache-stand-in";
        RuntimeDirectoryMode = "0700";
        PrivateNetwork = true;
        ExecStart = "${pkgs.python3}/bin/python3 ${serve} /run/cache-stand-in/cache.sock";
      };
    };
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    environment.systemPackages = [ pkgs.git pkgs.python3 pkgs.jq pkgs.tmux ];
    users.users.dev = { isNormalUser = true; uid = 1001; group = "dev"; };
    users.groups.dev.gid = 1001;
    virtualisation.memorySize = 2048;
  };
  nodes.machine = {
    environment.systemPackages = [ agents pkgs.git pkgs.bash pkgs.jq ];
    users.users.worker = { isNormalUser = true; uid = 1000; extraGroups = [ "wheel" ]; };
    users.users.${example.burrow.user} = { isNormalUser = true; uid = 1001; };
    environment.etc."codex/requirements.toml".source = example.environment.etc."codex/requirements.toml".source;
    security.sudo.wheelNeedsPassword = false;
    systemd.tmpfiles.rules = [ "d /work 0755 worker users -" ];
  };
  testScript = ''
    import shlex
    start_all()

    # A proj job on Codex, through proj's own argv to the pinned CLI.
    jobs.wait_for_unit("user@1001.service")
    jobs.wait_for_unit("gh-token-workers.socket")
    def dev(command):
        return jobs.succeed("su - dev -c " + shlex.quote(command)).strip()
    # The project is on a dataset the guard snapshots, which the user may
    # snapshot (burrow.undo's delegation, once the pool is there).
    jobs.succeed("zpool create -O mountpoint=none tank /dev/vdb && zfs create -o mountpoint=/data/dev/src/web tank/web")
    jobs.succeed("systemctl restart zfs-delegate-dev.service")
    jobs.succeed("install -d -o dev -g dev /data/dev/src/web /data/dev/elsewhere /srv/origin.git")
    # Git's global config is a file in the machine's /tmp, which the job's
    # commits need and the job's own /tmp would hide.
    dev("echo 'export GIT_CONFIG_GLOBAL=/tmp/dev-gitconfig' >>~/.bash_profile")
    dev("git config --global user.name Dev && git config --global user.email dev@example.test")
    jobs.succeed("grep -q dev@example.test /tmp/dev-gitconfig")
    dev("git init -q -b trunk /data/dev/src/web && git -C /data/dev/src/web commit -q --allow-empty -m initial")
    # Its origin is on the network, as GitHub is: the sandbox mounts nothing
    # of the machine's but what the job is given, /srv included.
    dev("git clone -q --bare /data/dev/src/web /srv/origin.git && git -C /data/dev/src/web remote add origin git://127.0.0.1/origin.git")
    dev("setsid -f git daemon --base-path=/srv --export-all --enable=receive-pack --listen=127.0.0.1 /srv >/dev/null 2>&1")
    jobs.wait_for_open_port(9418)
    # Something new upstream, for the job's fetch to bring.
    dev("git clone -q /srv/origin.git /tmp/upstream && git -C /tmp/upstream commit -q --allow-empty -m upstream && git -C /tmp/upstream push -q origin trunk")
    upstream = dev("git -C /tmp/upstream rev-parse HEAD")
    # The stand-in model, the CLI's own catalog entry for one, and a login
    # the job mustn't read.
    jobs.succeed("cp ${jobCalls} /tmp/calls.json && chmod 0644 /tmp/calls.json")
    jobs.succeed("${model} 18555 /tmp/calls.json >/tmp/model.log 2>&1 &")
    jobs.wait_for_open_port(18555)
    # The login shell moves Codex's home, through a chain of symlinks, after
    # proj has started the turn's unit: the deny must follow it there. The
    # chain starts in the machine's /tmp, which the job's own hides, so
    # Codex starts with where it ends; that's in the machine's /tmp too,
    # which Codex's own start keeps, or Codex would find no config, login
    # or session there.
    jobs.succeed("install -d -o dev -g dev -m 0700 /tmp/codex-home")
    dev("ln -s /tmp/codex-home ~/codex-hop && ln -s ~/codex-hop /tmp/codex-link && echo 'export CODEX_HOME=/tmp/codex-link' >>~/.bash_profile")
    assert dev("echo $CODEX_HOME") == "/tmp/codex-link"
    catalog = '{models: ([.models[] | select(.apply_patch_tool_type == "freeform" and .tool_mode == null)][:1] | map(.slug = "stand-in"))}'
    dev("codex debug models --bundled 2>/dev/null | jq " + shlex.quote(catalog) + " >~/catalog.json && jq -e '.models | length == 1' ~/catalog.json")
    # Top-level, so before the config's tables.
    dev("(echo 'model_catalog_json = \"'$HOME'/catalog.json\"' && echo 'default_permissions = \"proj\"' && cat ${modelConfig} ${widening}) >$CODEX_HOME/config.toml && chmod 0600 $CODEX_HOME/config.toml")
    dev("""echo '{"OPENAI_API_KEY": "sk-stand-in-login"}' >$CODEX_HOME/auth.json && chmod 0600 $CODEX_HOME/auth.json""")
    # Untracked, as a job's own write there would be.
    dev("mkdir /data/dev/src/web/.codex && cp ${projectWidening} /data/dev/src/web/.codex/config.toml")
    # What each turn deletes, with no snapshot of its dataset yet.
    dev("touch /data/dev/src/web/victim /data/dev/src/web/victim-again")
    jobs.fail("zfs list -H -t snapshot -o name tank/web | grep -q .")
    # tmux servers of the user's, outside any sandbox, where the job's env
    # wouldn't say: at a path of its own, and under a TMUX_TMPDIR of their
    # own (tmux falls back to /tmp for one that isn't there). tmux's default
    # dir isn't there yet.
    jobs.succeed("install -d -o dev -g dev /data/dev/tmux-custom")
    dev("tmux -S /tmp/served.sock new-session -d 'sleep infinity'")
    dev("TMUX_TMPDIR=/data/dev/tmux-custom tmux -L custom new-session -d 'sleep infinity'")
    # One bound by a relative name, which the kernel's table keeps as given.
    dev("cd /tmp && tmux -S served-rel.sock new-session -d 'sleep infinity'")
    jobs.succeed("test -S /tmp/served.sock && test -S /data/dev/tmux-custom/tmux-1001/custom && test -S /tmp/served-rel.sock")
    # The cache stand-in listens, though this namespace's socket table
    # doesn't list it (until a connect from here, whose server end the
    # kernel puts in the connecting side's namespace: so the user's own
    # connect, which shows it answers, comes after the job).
    jobs.wait_for_unit("cache-stand-in.service")
    jobs.wait_until_succeeds("test -S /run/cache-stand-in/cache.sock")
    jobs.fail("grep -q /run/cache-stand-in /proc/net/unix")
    # A Codex daemon of the user's, its socket where the CLI masks the dir.
    dev("install -d -m 0700 /tmp/codex-daemon-1001 && setsid -f python3 ${serve} /tmp/codex-daemon-1001/app.sock >/dev/null 2>&1")
    jobs.wait_until_succeeds("test -S /tmp/codex-daemon-1001/app.sock")
    dev(${builtins.toJSON (connect "/tmp/codex-daemon-1001/app.sock")})
    # A key the job mustn't read.
    dev("install -d -m 0700 ~/.ssh && echo secret-key-material >~/.ssh/id_test")
    jobs.fail("test -e /tmp/tmux-1001")
    dev("mkdir -p ~/.config/proj && echo 'coder codex' >~/.config/proj/roles")
    j = dev('proj run --role coder --app workers web "go"')
    # Once a command is running, the user starts two more: in tmux's default
    # dir, which must still take a server, and the custom one.
    # Or it ended without getting there, and the marks below say why.
    jobs.wait_until_succeeds(f"test -e /data/dev/jobs/{j}/waiting || su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
    dev("tmux -L host new-session -d 'sleep infinity'")
    dev("TMUX_TMPDIR=/data/dev/tmux-custom tmux -L late new-session -d 'sleep infinity'")
    jobs.succeed("test -S /tmp/tmux-1001/host && test -S /data/dev/tmux-custom/tmux-1001/late")
    jobs.succeed(f"touch /data/dev/jobs/{j}/late-started")
    jobs.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
    marks = dev(f"ls /data/dev/jobs/{j}").split()
    said = jobs.execute(f"cat /data/dev/jobs/{j}/1.jsonl /data/dev/jobs/{j}/1.stderr")[1]
    for mark in ["control-read-refused", "control-write-refused", "host-probed", "fetched", "committed", "connected", "no-new-privs", "elsewhere-refused", "login-refused",
                 "login-target-refused", "codex-config-refused", "gitconfig-read-only", "bus-refused", "manager-refused", "tmux-socket-refused",
                 "tmux-custom-refused", "tmux-refused", "tmux-late-refused", "tmux-relative-refused",
                 "netns-refused", "daemon-refused", "keys-refused", "tmp-own", "store-queried", "removed"]:
        assert mark in marks, (mark, marks, said)
    jobs.succeed("test -d /var/lib/burrow/proj-questions && test ! -e /var/lib/burrow/proj-questions/forged")
    assert jobs.succeed("readlink /run/current-system").strip() in dev(f"cat /data/dev/jobs/{j}/host-probe")
    # The file went, after the guard's undo snapshot of its dataset, which
    # still has it.
    def snapshots():
        return jobs.succeed("zfs list -H -t snapshot -s creation -o name tank/web").split()
    def kept(snapshot, name):
        assert snapshot.startswith("tank/web@pre-"), snapshot
        jobs.succeed(f"test -e /data/dev/src/web/.zfs/snapshot/{snapshot.split('@')[1]}/{name}")
    jobs.fail("test -e /data/dev/src/web/victim")
    first = snapshots()
    assert first, said
    kept(first[-1], "victim")
    # The job's own state, written by `proj done` inside the sandbox.
    assert dev(f"proj state {j}") == "done", (dev(f"proj state {j}"), said)
    # The fetch landed in the project's .git, and the worktree's commit, in
    # git's common dir, went through proj's hooks.
    assert dev("git -C /data/dev/src/web rev-parse origin/trunk") == upstream
    assert "Engine: codex" in dev("git -C /data/dev/src/web log -1 --format=%B work")
    for path in ["written", "by-socket", "by-custom", "by-tmux", "by-late", "by-relative"]:
        jobs.fail(f"test -e /data/dev/elsewhere/{path}")
    assert "sk-stand-in-login" not in said, said
    assert "secret-key-material" not in said, said
    # Its temp files went to its own /tmp, not the machine's.
    jobs.fail("test -e /tmp/job-tmp")
    assert "is ignored" not in said and "metadata for `stand-in` not found" not in said, said

    # A reply resumes it: a fresh start of Codex, under the same configs.
    jobs.succeed("cp ${bothCalls} /tmp/calls.json")
    dev(f'proj reply {j} "again"')
    jobs.wait_until_succeeds(f"test -e /data/dev/jobs/{j}/2.jsonl && su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
    marks = dev(f"ls /data/dev/jobs/{j}").split()
    said = jobs.execute(f"cat /data/dev/jobs/{j}/2.jsonl /data/dev/jobs/{j}/2.stderr")[1]
    for mark in ["resumed-elsewhere-refused", "resumed-login-refused", "resumed-codex-config-refused",
                 "resumed-committed", "resumed-bus-refused",
                 "resumed-tmux-refused", "resumed-tmux-relative-refused", "resumed-netns-refused",
                 "resumed-tmp-own", "resumed-removed"]:
        assert mark in marks, (mark, marks, said)
    jobs.fail("test -e /data/dev/src/web/victim-again")
    again = snapshots()
    assert len(again) > len(first), (first, again, said)
    kept(again[-1], "victim-again")
    assert dev(f"proj state {j}") == "done", (dev(f"proj state {j}"), said)
    for path in ["resumed", "resumed-by-tmux", "resumed-by-relative"]:
        jobs.fail(f"test -e /data/dev/elsewhere/{path}")
    jobs.fail("test -e /tmp/job-tmp")
    # The user's own servers ran on, all of them, untouched.
    dev("tmux -S /tmp/served.sock has-session")
    dev("tmux -S /tmp/served-rel.sock has-session")
    dev(${builtins.toJSON (connect "/run/cache-stand-in/cache.sock")})
    dev(${builtins.toJSON (connect "/tmp/codex-daemon-1001/app.sock")})
    for label in ["custom", "late"]:
        dev(f"TMUX_TMPDIR=/data/dev/tmux-custom tmux -L {label} has-session")
    dev("tmux -L host has-session")
    assert "sk-stand-in-login" not in said, said

    # A person's job, as proj-inbox starts one for a person's issue: no
    # --app. Their own gh login, and a token their profile exports, which
    # its sandbox must not reach.
    dev("install -d -m 0700 ~/.config/gh && printf 'github.com:\\n    oauth_token: gho_person-login\\n' >~/.config/gh/hosts.yml")
    dev("echo 'export GH_TOKEN=gho_person-profile' >>~/.bash_profile")
    assert dev("echo $GH_TOKEN") == "gho_person-profile"
    jobs.succeed("cp ${personJobCalls} /tmp/calls.json")
    p = dev('proj run --first --role coder --link https://github.com/sam/web/issues/1 web "go"')
    jobs.fail(f"test -e /data/dev/jobs/{p}/app")
    jobs.wait_until_succeeds(f"su - dev -c 'proj state {p}' | grep -Evx 'queued|running'", timeout=300)
    marks = dev(f"ls /data/dev/jobs/{p}").split()
    said = jobs.execute(f"cat /data/dev/jobs/{p}/1.jsonl /data/dev/jobs/{p}/1.stderr /data/dev/jobs/{p}/1.outcome")[1]
    for mark in ["person-connected", "person-helper", "person-login-refused", "person-pushed"]:
        assert mark in marks, (mark, marks, said)
    assert dev(f"proj state {p}") == "done", (dev(f"proj state {p}"), said)
    # Its branch is on origin, a commit its hooks marked.
    assert "Engine: codex" in dev("git -C /srv/origin.git log -1 --format=%B person")
    assert "gho_person" not in said, said

    machine.wait_for_unit("multi-user.target")
    def worker(command):
        return machine.succeed("su - worker -c " + shlex.quote(command))
    def configure(source):
        # Store files are read-only; both phases need a replaceable user config.
        worker(f"install -m 0600 {source} ~/.codex/config.toml && test -w ~/.codex/config.toml")
    worker("mkdir -p ~/.codex /work/scratch && git init -b trunk /work/project")
    worker("git -C /work/project -c user.name=Worker -c user.email=worker@example.test commit --allow-empty -m initial")
    worker("git -C /work/project worktree add -b work /work/scratch/tree")
    configure("${settings true}")
    sandbox = "codex sandbox -P proj -C /work/project -- "
    env = "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=${hooks} "
    worker(env + sandbox + "git -C /work/scratch/tree -c user.name=Worker -c user.email=worker@example.test commit --allow-empty -m changed")
    assert "Engine: codex" in worker("git -C /work/scratch/tree log -1 --format=%B")
    assert worker(sandbox + "grep NoNewPrivs /proc/self/status").split() == ["NoNewPrivs:", "1"]
    machine.fail("su - worker -c '" + sandbox + "sudo -n true'")
    worker("git clone --bare /work/project /work/remote")
    old = worker("git -C /work/project rev-parse HEAD").strip()
    new = worker("git -C /work/scratch/tree rev-parse HEAD").strip()
    hook = sandbox + "${hooks}/pre-push origin /work/remote"
    worker(f"echo 'refs/heads/work {new} refs/heads/trunk {old}' | " + hook)
    machine.fail("su - worker -c " + shlex.quote("echo '(delete) " + "0" * 40 + " refs/heads/trunk " + old + "' | " + hook))
    machine.fail("su - worker -c " + shlex.quote(f"echo 'refs/heads/work {old} refs/heads/trunk {new}' | " + hook))
    # Removing the metadata grant must stop the very worktree commit above.
    configure("${settings false}")
    machine.fail("su - worker -c '" + sandbox + "git -C /work/scratch/tree -c user.name=Worker -c user.email=worker@example.test commit --allow-empty -m denied'")

    # The guard, as Codex's managed hook, on what the stand-in model asks for.
    import json
    machine.succeed("echo '[]' > /work/calls && chmod 0644 /work/calls")
    machine.succeed("${model} 18555 /work/calls >/dev/null 2>&1 &")
    machine.wait_for_open_port(18555)
    # The stand-in, as a model with Codex's own apply_patch: the CLI's own
    # catalog entry for one, renamed.
    with_patch = '{models: ([.models[] | select(.apply_patch_tool_type == "freeform" and .tool_mode == null)][:1] | map(.slug = "stand-in"))}'
    machine.succeed("codex debug models --bundled 2>/dev/null | jq " + shlex.quote(with_patch) + " > /work/catalog.json")
    machine.succeed("jq -e '.models | length == 1' /work/catalog.json && chmod 0644 /work/catalog.json")
    def codex_run(user, calls):
        machine.succeed(f"echo {shlex.quote(json.dumps(calls))} > /work/calls")
        script = "mkdir -p ~/.codex ~/work && install -m 0600 ${modelConfig} ~/.codex/config.toml && cd ~/work && echo go | codex --no-daemon exec -c model_catalog_json=/work/catalog.json --skip-git-repo-check --sandbox workspace-write --json -"
        return machine.succeed(f"su - {user} -c {shlex.quote(script)} 2>&1")
    def run(command):
        return [["exec_command", {"cmd": command}]]
    def patch(body):
        return [["apply_patch", {"input": "*** Begin Patch\n" + body + "\n*** End Patch\n"}]]
    job = "${example.burrow.user}"
    machine.succeed(f"su - {job} -c 'mkdir -p ~/work && touch ~/work/victim && git init -q ~/work'")
    # No undo snapshot here (no ZFS), so what destroys files is refused.
    said = codex_run(job, run("rm victim"))
    assert "blocked by PreToolUse hook" in said and "undo snapshot" in said, said
    machine.succeed(f"su - {job} -c 'test -e ~/work/victim'")
    # A commit its hooks wouldn't mark.
    said = codex_run(job, run("git -c core.hooksPath=/dev/null -c user.name=Job -c user.email=job@example.test commit --allow-empty -m unmarked"))
    assert "blocked by PreToolUse hook" in said, said
    machine.fail(f"su - {job} -c 'git -C ~/work log'")
    # Nothing typed into a shell gets past the hook: no TTY, no open stdin.
    codex_run(job, [
        ["exec_command", {"cmd": "bash", "tty": True, "yield_time_ms": 500}],
        ["write_stdin", {"session_id": "SID", "chars": "rm victim\n", "yield_time_ms": 1000}],
    ])
    codex_run(job, [
        ["exec_command", {"cmd": "bash", "yield_time_ms": 500}],
        ["write_stdin", {"session_id": "SID", "chars": "rm victim\n", "yield_time_ms": 1000}],
    ])
    machine.succeed(f"su - {job} -c 'test -e ~/work/victim'")
    # Codex's own apply_patch deletes with no command: no undo, so refused,
    # and so is the same patch a command hands Codex to apply.
    said = codex_run(job, patch("*** Delete File: victim"))
    assert "blocked by PreToolUse hook" in said and "undo snapshot" in said, said
    said = codex_run(job, run("apply_patch <<'EOF'\n*** Begin Patch\n*** Delete File: victim\n*** End Patch\nEOF"))
    assert "blocked by PreToolUse hook" in said and "undo snapshot" in said, said
    machine.succeed(f"su - {job} -c 'test -e ~/work/victim'")
    # A patch that only adds a file has nothing to undo, and goes through.
    said = codex_run(job, patch("*** Add File: added\n+new"))
    machine.succeed(f"su - {job} -c 'test -e ~/work/added'")
    # The control: the same command and patch, by a user whose Codex isn't
    # a job's.
    machine.succeed("su - worker -c 'mkdir -p ~/work && touch ~/work/victim'")
    said = codex_run("worker", run("rm victim"))
    assert "blocked by PreToolUse hook" not in said, said
    machine.fail("su - worker -c 'test -e ~/work/victim'")
    machine.succeed("su - worker -c 'touch ~/work/victim'")
    said = codex_run("worker", patch("*** Delete File: victim"))
    assert "blocked by PreToolUse hook" not in said, said
    machine.fail("su - worker -c 'test -e ~/work/victim'")
  '';
}
