# Exercise the pinned CLI's actual filesystem policy, without an API call:
# worktree commits need an explicit grant to the clone's git metadata. Then
# its hooks, with a stand-in model that asks for one command: the example
# machine's /etc/codex/requirements.toml runs the guard on every command a
# Codex job (as the machine's user) runs, and refuses what has no undo here
# (no ZFS in this VM, so no snapshot) or would hide a commit from the job's
# hooks; another user's Codex runs the same command unguarded.
{ self, pkgs }:
let
  example = self.nixosConfigurations.example.config;
  model = ./proj-codex-sandbox/model.py;
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
in
pkgs.testers.runNixOSTest {
  name = "proj-codex-sandbox";
  nodes.machine = {
    environment.systemPackages = [ agents pkgs.git pkgs.bash ];
    users.users.worker = { isNormalUser = true; uid = 1000; extraGroups = [ "wheel" ]; };
    users.users.${example.burrow.user} = { isNormalUser = true; uid = 1001; };
    environment.etc."codex/requirements.toml".source = example.environment.etc."codex/requirements.toml".source;
    security.sudo.wheelNeedsPassword = false;
    systemd.tmpfiles.rules = [ "d /work 0755 worker users -" ];
  };
  testScript = ''
    import shlex
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
    machine.succeed("${pkgs.python3}/bin/python3 ${model} 18555 /work/calls >/dev/null 2>&1 &")
    machine.wait_for_open_port(18555)
    def codex_run(user, calls):
        machine.succeed(f"echo {shlex.quote(json.dumps(calls))} > /work/calls")
        script = "mkdir -p ~/.codex ~/work && install -m 0600 ${modelConfig} ~/.codex/config.toml && cd ~/work && echo go | codex --no-daemon exec --skip-git-repo-check --sandbox workspace-write --json -"
        return machine.succeed(f"su - {user} -c {shlex.quote(script)} 2>&1")
    def run(command):
        return [["exec_command", {"cmd": command}]]
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
    # The control: the same command, by a user whose Codex isn't a job's.
    machine.succeed("su - worker -c 'mkdir -p ~/work && touch ~/work/victim'")
    said = codex_run("worker", run("rm victim"))
    assert "blocked by PreToolUse hook" not in said, said
    machine.fail("su - worker -c 'test -e ~/work/victim'")
  '';
}
