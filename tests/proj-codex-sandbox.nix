# Exercise the pinned CLI's actual filesystem policy, without an API call:
# worktree commits need an explicit grant to the clone's git metadata.
{ self, pkgs }:
let
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
  '';
}
