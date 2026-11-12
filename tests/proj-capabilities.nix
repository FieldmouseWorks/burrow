{ self, pkgs }:
let
  fixture = self.packages.${pkgs.stdenv.hostPlatform.system}.codex-stand-in;
  agents = import ../modules/autoland/agents/package.nix { inherit pkgs; inherit (self.inputs) nixpkgs-unstable; };
  model = "${self.packages.${pkgs.stdenv.hostPlatform.system}.codex-stand-in}/bin/codex-stand-in";
  claudeClient = import ../modules/claude/package.nix {
    inputs = self.inputs;
    system = pkgs.stdenv.hostPlatform.system;
  };
  dropClaude = pkgs.writeShellScriptBin "claude" ''
    exec ${fixture}/bin/capabilities drop-claude-mcp ${claudeClient}/bin/claude "$@"
  '';
  claudeCalls = pkgs.writeText "claude-capability-calls.json" (builtins.toJSON [
    [ "Bash" { command = "${fixture}/bin/capabilities"; timeout = 300000; } ]
    [ "mcp__reach__ping" { } ]
  ]);
  emptyCalls = pkgs.writeText "host-target-calls.json" "[]";
  fakeGh = pkgs.writeShellScriptBin "gh" ''
    exec ${fixture}/bin/capabilities github "$@"
  '';
  drop = pkgs.writeShellScript "codex-without-heavy" ''
    exec ${fixture}/bin/capabilities drop-heavy ${agents}/bin/codex "$@"
  '';
  dropJobs = pkgs.writeShellScript "codex-without-host-jobs" ''
    exec ${fixture}/bin/capabilities drop-host-jobs ${agents}/bin/codex "$@"
  '';
  calls = pkgs.writeText "capability-calls.json" (builtins.toJSON [
    [ "exec_command" { cmd = "${fixture}/bin/capabilities"; yield_time_ms = 300000; } ]
    [ "mcp__reach.ping" { } ]
  ]);
  config = pkgs.writeText "stand-in.toml" ''
    model = "stand-in"
    model_provider = "stand-in"
    [model_providers.stand-in]
    name = "stand-in"
    base_url = "http://127.0.0.1:18555/v1"
    wire_api = "responses"
    [mcp_servers.outside]
    command = "${fixture}/bin/capabilities"
    args = ["outside"]
    [mcp_servers.reach]
    command = "${fixture}/bin/capabilities"
    args = ["outside"]
    enabled = false
    [mcp_servers.reach.env]
    EXTRA_SERVER_ONLY = "outside"
  '';
in pkgs.testers.runNixOSTest {
  name = "proj-capabilities";
  nodes.machine = {
    imports = [ self.nixosModules.proj self.nixosModules.cpu ];
    burrow = {
      claude.enable = true;
      cpu.enable = true;
      proj = { enable = true; minFreeMemory = 0; gh = fakeGh; meters = "/var/lib/job-meters"; };
      mcpServers.reach = {
        command = "${fixture}/bin/capabilities";
        args = [ "server" ];
        env.MCP_ENGINE = "1";
        envVars = [ "PROJ_JOB" ];
        readPaths = [ "/run/reach" ];
      };
      github.apps.workers = {
        id = 7; accountId = 8; slug = "test-workers"; keyFile = "/run/keys/workers.pem"; users = [ "dev" ];
      };
      github.machinery = "workers";
    };
    users.users.dev = { isNormalUser = true; uid = 1001; group = "dev"; extraGroups = [ "kvm" ]; };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /var/lib/job-meters 0755 root root -"
      "f /var/lib/job-meters/reach 0644 root root - facts"
    ];
    environment.systemPackages = [ pkgs.git pkgs.jq fixture ];
    environment.etc."capability-build.nix".text = ''
      { job }: derivation {
        name = "job-build-" + job;
        system = builtins.currentSystem;
        builder = (builtins.storePath "${pkgs.busybox}") + "/bin/sh";
        args = [ "-c" "echo built > $out" ];
      }
    '';
    # An MCP dependency in a root-owned socket directory, outside /run/user.
    systemd.services.reach = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = { RuntimeDirectory = "reach"; UMask = "0000"; ExecStart = "${fixture}/bin/capabilities listener"; };
    };
    systemd.services.burrow-events.enable = false;
    systemd.timers.burrow-events-catch-up.enable = false;
    virtualisation = { memorySize = 3072; cores = 2; };
  };
  testScript = ''
    import json, shlex
    start_all()
    machine.wait_for_unit("user@1001.service")
    machine.wait_for_unit("gh-token-workers.socket")
    machine.wait_for_unit("reach.service")
    def dev(command):
        return machine.succeed("su - dev -c " + shlex.quote(command)).strip()
    machine.succeed("install -d -o dev -g dev /data/dev/src/web /data/dev/src/target /srv/origin.git")
    dev("git config --global user.name Dev && git config --global user.email dev@example.test")
    dev("git init -q -b trunk /data/dev/src/web && git -C /data/dev/src/web commit -q --allow-empty -m initial && git clone -q --bare /data/dev/src/web /srv/origin.git && git -C /data/dev/src/web remote add origin git://127.0.0.1/origin.git")
    dev("setsid -f git daemon --base-path=/srv --export-all --enable=receive-pack --listen=127.0.0.1 /srv >/tmp/git.log 2>&1")
    machine.wait_for_open_port(9418)
    machine.succeed("${model} 18555 ${calls} >/tmp/model.log 2>&1 &")
    machine.wait_for_open_port(18555)
    machine.succeed("${model} 18556 ${claudeCalls} >/tmp/claude-provider.log 2>&1 &")
    machine.wait_for_open_port(18556)
    machine.succeed("${model} 18557 ${emptyCalls} >/tmp/host-target-provider.log 2>&1 &")
    machine.wait_for_open_port(18557)
    dev("mkdir -p ~/.codex ~/.config/proj && printf 'first claude\\nsecond codex\\n' >~/.config/proj/roles")
    catalog = '{models: ([.models[] | select(.apply_patch_tool_type == "freeform" and .tool_mode == null)][:1] | map(.slug = "stand-in"))}'
    dev("codex debug models --bundled 2>/dev/null | jq " + shlex.quote(catalog) + " >~/catalog.json")
    dev("(echo 'model_catalog_json = \"'$HOME'/catalog.json\"' && cat ${config}) >~/.codex/config.toml")
    dev("""echo '{"OPENAI_API_KEY":"sk-stand-in"}' >~/.codex/auth.json""")
    # This check is about reach, not GitHub's remote API: keep the real
    # App environment and token socket and stand in only for gh's endpoint.
    dev("echo 'export PATH=${fakeGh}/bin:$PATH ANTHROPIC_BASE_URL=http://127.0.0.1:18557 ANTHROPIC_API_KEY=sk-stand-in ANTHROPIC_MODEL=stand-in CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1' >>~/.bash_profile")
    # Seed a real resumable conversation, then model a timed-out first turn.
    # Its host history is outside the Codex sandbox. Both engines must
    # continue it through the ordinary CLI and get a new engine result.
    original = dev("sed -n 's/^export PROJ_CONFIG=\\([^ ]*\\).*/\\1/p' $(command -v proj)")
    # Ordinary jobs reserve their project's last slot for steward work. A
    # machine recovery and its target belong to different projects, so the
    # caller can await the resumed target without holding its project slot.
    target_id = dev('proj run --local-work --role first --app workers target "seed a resumable conversation"')
    machine.wait_until_succeeds(f"su - dev -c 'proj state {target_id}' | grep -Evx 'queued|running'", timeout=300)
    target = "/data/dev/jobs/" + target_id
    machine.succeed("test -s " + target + "/session")
    dev("printf %s 'timed out after 60m' >" + target + "/1.outcome")
    dev("printf '%s\\n' " + shlex.quote('{"type":"fixture","note":"host recovery transcript"}') + " >>" + target + "/1.jsonl")
    profile = f'export PROJ_RECOVERY_TARGET={target_id}\nif [ "$PROJ_JOB" != "{target_id}" ]; then export ANTHROPIC_BASE_URL=http://127.0.0.1:18556; fi\n'
    dev("printf %s " + shlex.quote(profile) + " >>~/.bash_profile")
    def job(role):
        j = dev(f'proj run --local-work --role {role} --app workers web go')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {j}' | grep -Evx 'queued|running'", timeout=300)
        return j
    def results(j):
        return machine.execute(f"cat /data/dev/jobs/{j}/capabilities-passed")[1].splitlines()
    a, b = job("first"), job("second")
    expected = json.loads(machine.succeed(f"cat /data/dev/jobs/{a}/capabilities-declared"))
    assert results(a) == expected, (expected, results(a), machine.execute(f"cat /data/dev/jobs/{a}/1.jsonl"))
    assert dev(f"proj state {a}") == "done"
    assert expected and results(b) == expected, (a, b, expected, results(b), machine.execute(f"cat /data/dev/jobs/{b}/1.jsonl /data/dev/jobs/{b}/1.stderr"))
    assert dev(f"proj state {b}") == "done"
    for j in [a, b]:
        machine.succeed(f"test -f /data/dev/jobs/{j}/host-job-recovered")
        assert machine.succeed("readlink /run/current-system").strip() in machine.succeed(f"cat /data/dev/jobs/{j}/host-probe")
        assert machine.execute(f"test -f /data/dev/jobs/{j}/mcp-called")[0] == 0, machine.execute(f"cat /data/dev/jobs/{j}/1.jsonl /data/dev/jobs/{j}/1.stderr /tmp/model.log /tmp/claude-provider.log")
    machine.fail("test -e /data/dev/src/web/unexpected-server")
    assert int(machine.succeed("cat " + target + "/turns")) == 3
    # The real client must supply the evidence: the direct probe still runs
    # when its engine registry is empty, but cannot satisfy mcp-called.
    dev("echo 'export PATH=${dropClaude}/bin:$PATH' >>~/.bash_profile")
    missing_mcp = job("first")
    assert results(missing_mcp) == expected
    machine.succeed(f"test -f /data/dev/jobs/{missing_mcp}/mcp-direct")
    machine.fail(f"test -f /data/dev/jobs/{missing_mcp}/mcp-called")
    print("Negative control passed: removing Claude's rendered MCP registry broke engine-call evidence despite the direct probe succeeding")
    # Drop the heavy grant from Codex's generated profile, leaving the
    # declaration and probe unchanged. The same comparison must go red.
    def without(name, executable):
        path = "/home/dev/" + name
        update = '.tools.codex = ' + json.dumps(executable) + ' | .deployed = ' + json.dumps(path)
        dev("jq " + shlex.quote(update) + " " + original + " >" + path + ".json")
        wrapper = f"#!/bin/sh\nexport PROJ_CONFIG={path}.json PROJ_SELF={path}\nexec ${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj \"$@\"\n"
        dev("printf %s " + shlex.quote(wrapper) + " >" + path + " && chmod +x " + path)
        n = dev(path + ' run --local-work --role second --app workers web go')
        machine.wait_until_succeeds(f"su - dev -c 'proj state {n}' | grep -Evx 'queued|running'", timeout=300)
        return n
    no_jobs = without("proj-without-host-jobs", "${dropJobs}")
    assert results(no_jobs) == expected[:expected.index("job-state")], "dropping host job access did not break parity"
    assert machine.succeed(f"cat /data/dev/jobs/{no_jobs}/capability-running").strip() == "job-state"
    print("Negative control passed: dropping the host job grant broke recovery from the Codex sandbox")
    n = without("proj-negative", "${drop}")
    assert results(n) != expected, "dropping heavy did not break parity"
    assert machine.succeed(f"cat /data/dev/jobs/{n}/capability-running").strip() == "heavy"
    assert results(n) == expected[:expected.index("heavy")]
    print("Negative control passed: dropping Codex's heavy grant failed the heavy probe and broke parity")
  '';
}
