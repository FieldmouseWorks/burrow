{ self, pkgs }:
let
  fixture = pkgs.runCommand "job-capability-fixture" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2024 ${./proj/capabilities.rs} -o $out/bin/capabilities
  '';
  agents = import ../modules/autoland/agents/package.nix { inherit pkgs; inherit (self.inputs) nixpkgs-unstable; };
  model = "${self.packages.${pkgs.stdenv.hostPlatform.system}.codex-stand-in}/bin/codex-stand-in";
  # Claude's CLI stand-in runs the very same tool executable as Codex's
  # stand-in provider. Its argv checks the engine's MCP configuration too.
  claude = pkgs.writeShellScriptBin "claude" ''
    set -e
    [[ " $* " == *" --strict-mcp-config "* ]]
    configured=false
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --mcp-config ]; then
        configured=true
        shift
        jq -e --arg job "$PROJ_JOB" '.mcpServers.reach.command == "${fixture}/bin/capabilities" and .mcpServers.reach.args == ["server"] and .mcpServers.reach.env.PROJ_JOB == $job and .mcpServers.reach.env.MCP_ENGINE == "1"' <<<"$1" >/dev/null
      fi
      shift
    done
    "$configured"
    cat >/dev/null
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"tools/call"}' | MCP_ENGINE=1 ${fixture}/bin/capabilities server >/dev/null
    ${fixture}/bin/capabilities 2>&1
    echo '{"type":"result","session_id":"reach","result":"done","is_error":false}'
  '';
  fakeGh = pkgs.writeShellScriptBin "gh" ''
    exec ${fixture}/bin/capabilities github "$@"
  '';
  drop = pkgs.writeShellScript "codex-without-heavy" ''
    exec ${fixture}/bin/capabilities drop-heavy ${agents}/bin/codex "$@"
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
  '';
in pkgs.testers.runNixOSTest {
  name = "proj-capabilities";
  nodes.machine = {
    imports = [ self.nixosModules.proj self.nixosModules.cpu ];
    burrow = {
      claude.package = claude;
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
        id = 7; slug = "test-workers"; keyFile = "/run/keys/workers.pem"; users = [ "dev" ];
      };
      github.machinery = "workers";
    };
    users.users.dev = { isNormalUser = true; uid = 1001; group = "dev"; extraGroups = [ "kvm" ]; };
    users.groups.dev.gid = 1001;
    systemd.tmpfiles.rules = [
      "d /var/lib/job-meters 0755 root root -"
      "f /var/lib/job-meters/reach 0644 root root - facts"
    ];
    environment.systemPackages = [ pkgs.git pkgs.jq pkgs.python3 fixture ];
    environment.etc."capability-build.nix".text = ''
      derivation {
        name = "job-build";
        system = builtins.currentSystem;
        builder = (builtins.storePath "${pkgs.busybox}") + "/bin/sh";
        args = [ "-c" "echo built > $out" ];
      }
    '';
    # An MCP dependency in a root-owned socket directory, outside /run/user.
    systemd.services.reach = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig = { RuntimeDirectory = "reach"; UMask = "0000"; ExecStart = "${pkgs.python3}/bin/python3 -c 'import socket,time; s=socket.socket(socket.AF_UNIX); s.bind(\"/run/reach/socket\"); s.listen(100); time.sleep(10000)'"; };
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
    machine.succeed("install -d -o dev -g dev /data/dev/src/web /srv/origin.git")
    dev("git config --global user.name Dev && git config --global user.email dev@example.test")
    dev("git init -q -b trunk /data/dev/src/web && git -C /data/dev/src/web commit -q --allow-empty -m initial && git clone -q --bare /data/dev/src/web /srv/origin.git && git -C /data/dev/src/web remote add origin git://127.0.0.1/origin.git")
    dev("setsid -f git daemon --base-path=/srv --export-all --enable=receive-pack --listen=127.0.0.1 /srv >/tmp/git.log 2>&1")
    machine.wait_for_open_port(9418)
    machine.succeed("${model} 18555 ${calls} >/tmp/model.log 2>&1 &")
    machine.wait_for_open_port(18555)
    dev("mkdir -p ~/.codex ~/.config/proj && printf 'first claude\\nsecond codex\\n' >~/.config/proj/roles")
    catalog = '{models: ([.models[] | select(.apply_patch_tool_type == "freeform" and .tool_mode == null)][:1] | map(.slug = "stand-in"))}'
    dev("codex debug models --bundled 2>/dev/null | jq " + shlex.quote(catalog) + " >~/catalog.json")
    dev("(echo 'model_catalog_json = \"'$HOME'/catalog.json\"' && cat ${config}) >~/.codex/config.toml")
    dev("""echo '{"OPENAI_API_KEY":"sk-stand-in"}' >~/.codex/auth.json""")
    # This check is about reach, not GitHub's remote API: keep the real
    # App environment and token socket and stand in only for gh's endpoint.
    dev("echo 'export PATH=${fakeGh}/bin:$PATH' >>~/.bash_profile")
    def job(role):
        j = dev(f'proj run --role {role} --app workers web go')
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
        machine.succeed(f"test -f /data/dev/jobs/{j}/mcp-called")
    machine.fail("test -e /data/dev/src/web/unexpected-server")
    # Drop the heavy grant from Codex's generated profile, leaving the
    # declaration and probe unchanged. The same comparison must go red.
    original = dev("sed -n 's/^export PROJ_CONFIG=\\([^ ]*\\).*/\\1/p' $(command -v proj)")
    dev("jq '.tools.codex = \"${drop}\" | .deployed = \"/data/dev/proj-negative\"' " + original + " >/data/dev/proj-negative.json")
    wrapper = "#!/bin/sh\nexport PROJ_CONFIG=/data/dev/proj-negative.json PROJ_SELF=/data/dev/proj-negative\nexec ${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-proj}/bin/burrow-proj \"$@\"\n"
    machine.succeed("printf %s " + shlex.quote(wrapper) + " >/data/dev/proj-negative && chmod +x /data/dev/proj-negative && chown dev:dev /data/dev/proj-negative*")
    n = dev('/data/dev/proj-negative run --role second --app workers web go')
    machine.wait_until_succeeds(f"su - dev -c 'proj state {n}' | grep -Evx 'queued|running'", timeout=300)
    assert results(n) != expected, "dropping heavy did not break parity"
    assert machine.succeed(f"cat /data/dev/jobs/{n}/capability-running").strip() == "heavy"
    assert results(n) == expected[:expected.index("heavy")]
    print("Negative control passed: dropping Codex's heavy grant failed the heavy probe and broke parity")
  '';
}
