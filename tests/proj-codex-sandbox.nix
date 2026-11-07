# Exercise the pinned CLI's actual filesystem policy, without an API call:
# worktree commits need an explicit grant to the clone's git metadata. Then
# a proj job on Codex, for real (the `jobs` machine): proj's own argv to the
# pinned CLI, with a stand-in model that asks for what a job does, and what
# the job's sandbox let through. Then
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
  # (through its symlink, and at its target), and reach what runs things
  # outside the sandbox: the user's D-Bus and the systemd manager's private
  # socket, spoken to directly, and a tmux server of the user's. No redirects: the guard wants an undo
  # snapshot for those, and there's no ZFS here.
  connect = path: "python3 -c 'import socket; socket.socket(socket.AF_UNIX).connect(\"${path}\")'";
  calls = cmds: map (cmd: [ "exec_command" { inherit cmd; } ]) cmds;
  firstCalls = calls [
    "git fetch -q origin && touch /data/dev/jobs/$PROJ_JOB/fetched"
    "git worktree add -q -b work \"$PROJ_SCRATCH/tree\" origin/trunk && git -C \"$PROJ_SCRATCH/tree\" commit -q --allow-empty -m changed && touch /data/dev/jobs/$PROJ_JOB/committed"
    "python3 -c 'import os, socket; socket.socket(socket.AF_UNIX).connect(os.environ[\"GH_TOKEN_SOCKET\"])' && touch /data/dev/jobs/$PROJ_JOB/connected"
    "grep -qx 'NoNewPrivs:[[:space:]]1' /proc/self/status && touch /data/dev/jobs/$PROJ_JOB/no-new-privs"
    "touch /data/dev/elsewhere/written || touch /data/dev/jobs/$PROJ_JOB/elsewhere-refused"
    "cat \"$CODEX_HOME/auth.json\" || touch /data/dev/jobs/$PROJ_JOB/login-refused"
    "cat /data/dev/codex-home/auth.json || touch /data/dev/jobs/$PROJ_JOB/login-target-refused"
    "${connect "/run/user/1001/bus"} || touch /data/dev/jobs/$PROJ_JOB/bus-refused"
    "${connect "/run/user/1001/systemd/private"} || touch /data/dev/jobs/$PROJ_JOB/manager-refused"
    "tmux -L host new-window 'touch /data/dev/elsewhere/by-tmux' || touch /data/dev/jobs/$PROJ_JOB/tmux-refused"
    "proj done"
  ];
  # Its next turn, a resumed Codex with a fresh profile, held to the same.
  resumedCalls = calls [
    "touch /data/dev/elsewhere/resumed || touch /data/dev/jobs/$PROJ_JOB/resumed-elsewhere-refused"
    "cat \"$CODEX_HOME/auth.json\" || touch /data/dev/jobs/$PROJ_JOB/resumed-login-refused"
    "${connect "/run/user/1001/bus"} || touch /data/dev/jobs/$PROJ_JOB/resumed-bus-refused"
    "proj done"
  ];
  jobCalls = pkgs.writeText "codex-job-calls.json" (builtins.toJSON firstCalls);
  bothCalls = pkgs.writeText "codex-job-calls-resumed.json" (builtins.toJSON (firstCalls ++ resumedCalls));
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
  # proj, its Codex role and the workers App, as a machine runs them.
  nodes.jobs = {
    imports = [ self.nixosModules.proj ];
    burrow = {
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
    jobs.succeed("install -d -o dev -g dev /data/dev/src/web /data/dev/elsewhere /srv/origin.git")
    dev("git config --global user.name Dev && git config --global user.email dev@example.test")
    dev("git init -q -b trunk /data/dev/src/web && git -C /data/dev/src/web commit -q --allow-empty -m initial")
    dev("git clone -q --bare /data/dev/src/web /srv/origin.git && git -C /data/dev/src/web remote add origin /srv/origin.git")
    # Something new upstream, for the job's fetch to bring.
    dev("git clone -q /srv/origin.git /tmp/upstream && git -C /tmp/upstream commit -q --allow-empty -m upstream && git -C /tmp/upstream push -q origin trunk")
    upstream = dev("git -C /tmp/upstream rev-parse HEAD")
    # The stand-in model, the CLI's own catalog entry for one, and a login
    # the job mustn't read.
    jobs.succeed("cp ${jobCalls} /tmp/calls.json && chmod 0644 /tmp/calls.json")
    jobs.succeed("${model} 18555 /tmp/calls.json >/tmp/model.log 2>&1 &")
    jobs.wait_for_open_port(18555)
    # The login shell moves Codex's home, through a symlink, after proj has
    # started the turn's unit: the deny must follow it there.
    jobs.succeed("install -d -o dev -g dev -m 0700 /data/dev/codex-home")
    dev("ln -s /data/dev/codex-home ~/codex-link && echo 'export CODEX_HOME=$HOME/codex-link' >>~/.bash_profile")
    assert dev("echo $CODEX_HOME") == "/home/dev/codex-link"
    catalog = '{models: ([.models[] | select(.apply_patch_tool_type == "freeform" and .tool_mode == null)][:1] | map(.slug = "stand-in"))}'
    dev("codex debug models --bundled 2>/dev/null | jq " + shlex.quote(catalog) + " >~/catalog.json && jq -e '.models | length == 1' ~/catalog.json")
    # Top-level, so before the config's tables.
    dev("(echo 'model_catalog_json = \"'$HOME'/catalog.json\"' && echo 'default_permissions = \"proj\"' && cat ${modelConfig} ${widening}) >$CODEX_HOME/config.toml && chmod 0600 $CODEX_HOME/config.toml")
    dev("""echo '{"OPENAI_API_KEY": "sk-stand-in-login"}' >$CODEX_HOME/auth.json && chmod 0600 $CODEX_HOME/auth.json""")
    # Untracked, as a job's own write there would be.
    dev("mkdir /data/dev/src/web/.codex && cp ${projectWidening} /data/dev/src/web/.codex/config.toml")
    # A tmux server of the user's, outside any sandbox.
    dev("tmux -L host new-session -d 'sleep infinity'")
    dev("mkdir -p ~/.config/proj && echo 'coder codex' >~/.config/proj/roles")
    j = dev('proj run --role coder --app workers web "go"')
    jobs.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
    marks = dev(f"ls /data/dev/jobs/{j}").split()
    said = jobs.execute(f"cat /data/dev/jobs/{j}/1.jsonl /data/dev/jobs/{j}/1.stderr")[1]
    for mark in ["fetched", "committed", "connected", "no-new-privs", "elsewhere-refused", "login-refused",
                 "login-target-refused", "bus-refused", "manager-refused", "tmux-refused"]:
        assert mark in marks, (mark, marks, said)
    # The job's own state, written by `proj done` inside the sandbox.
    assert dev(f"proj state {j}") == "done", (dev(f"proj state {j}"), said)
    # The fetch landed in the project's .git, and the worktree's commit, in
    # git's common dir, went through proj's hooks.
    assert dev("git -C /data/dev/src/web rev-parse origin/trunk") == upstream
    assert "Engine: codex" in dev("git -C /data/dev/src/web log -1 --format=%B work")
    for path in ["written", "by-tmux"]:
        jobs.fail(f"test -e /data/dev/elsewhere/{path}")
    assert "sk-stand-in-login" not in said, said
    assert "is ignored" not in said and "metadata for `stand-in` not found" not in said, said

    # A reply resumes it: a fresh start of Codex, under the same configs.
    jobs.succeed("cp ${bothCalls} /tmp/calls.json")
    dev(f'proj reply {j} "again"')
    jobs.wait_until_succeeds(f"test -e /data/dev/jobs/{j}/2.jsonl && su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
    marks = dev(f"ls /data/dev/jobs/{j}").split()
    said = jobs.execute(f"cat /data/dev/jobs/{j}/2.jsonl /data/dev/jobs/{j}/2.stderr")[1]
    for mark in ["resumed-elsewhere-refused", "resumed-login-refused", "resumed-bus-refused"]:
        assert mark in marks, (mark, marks, said)
    assert dev(f"proj state {j}") == "done", (dev(f"proj state {j}"), said)
    jobs.fail("test -e /data/dev/elsewhere/resumed")
    assert "sk-stand-in-login" not in said, said

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
