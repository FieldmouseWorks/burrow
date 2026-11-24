# An engine plugin's conformance check: a VM test that runs the plugin in
# its engine's sandbox, as burrow.agents.engines would on a machine, and
# holds it to Conary's contract and to what it declares. docs/engines.md,
# "A plugin's conformance check", says what it checks.
#
#   self.lib.engineConformance {
#     inherit pkgs;
#     name = "example";       # its engine's name
#     plugin = package;       # its main program speaks the plugin contract
#     capabilities = [ ... ]; # what its owner approves (the engine's list)
#     engine = { };           # more of its engine's entry: packages, say
#   }
#
# It needs /dev/kvm, nested: CI only evaluates it (checks/agents.nix).
{ self }:
{ pkgs, name, plugin, capabilities, engine ? { } }:
let
  inherit (pkgs) lib;
  inherit (import ../tests/agents/capabilities.nix) grantable withheld;
  example = self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin;
  fixture = pkgs.runCommand "${name}-conformance-fixture" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "conformance-fixture";
  } ''
    makeBinaryWrapper ${lib.getExe' example "burrow-example-plugin"} $out/bin/conformance-fixture \
      --set EXAMPLE_PLUGIN_AROUND ${lib.getExe plugin}
  '';
  # The plugin as the owner runs it, and the fixture around it with the
  # owner's list and with every name a list can hold.
  engines = {
    ${name} = { inherit plugin capabilities; proxyPort = 3141; };
    "${name}-owner" = { plugin = fixture; inherit capabilities; proxyPort = 3142; };
    "${name}-all" = { plugin = fixture; capabilities = grantable ++ withheld; proxyPort = 3143; };
  };
in
pkgs.testers.runNixOSTest {
  name = "engine-conformance-${name}";

  nodes.host = {
    imports = [ (import ../tests/agents/plugin-host.nix { inherit self; }) ];
    burrow.agents.engines = lib.mapAttrs (_: own: lib.mkForce ({
      packages = [ ];
      guestModules = [ ];
      credentialFile = null;
      credentialEnv = null;
    } // engine // own // {
      hosts = (engine.hosts or [ ]) ++ [ "conformance.example" ];
      mcpServers = (engine.mcpServers or [ ]) ++ [ "notes" ];
    })) engines;
    networking.hosts."127.0.0.1" = [ "conformance.example" ];
  };

  testScript = ''
    import json, shlex

    NAME = "${name}"
    ENGINES = ${builtins.toJSON (builtins.attrNames engines)}
    DEMO = "/data/dev/src/demo"
    BIN = "/run/current-system/sw/bin"
    CALL = f"{BIN}/burrow-example-plugin-call"
    # As long as several nested boots may take on a loaded machine.
    LONG = 3600

    host.wait_for_unit("multi-user.target")
    host.wait_for_unit("agents-dataset.service")
    host.wait_for_unit("reach-tcp.service")
    for engine in ENGINES:
        host.wait_for_unit(f"agent-proxy-{engine}.service")
        host.wait_for_unit(f"agent-plugin-{engine}.socket")

    def dev(cmd):
        return host.succeed(f"su - dev -c {shlex.quote(cmd)}")

    git = "git -c user.name=Test -c user.email=test@example.com"
    dev(f"mkdir {DEMO} && cd {DEMO} && git init -q -b main && echo one >a && git add a && {git} commit -qm one")

    look = {"allowed": ["conformance.example", "engine-reached"]}
    host.succeed(f"printf %s {shlex.quote(json.dumps(look))} >/tmp/look")

    def step(what, command):
        return f"{command} >/tmp/{what}.out 2>/tmp/{what}.err; echo $? >/tmp/{what}.exit"

    approvals = {"owner-approved": f"{NAME}-owner", "all-approved": f"{NAME}-all"}
    script = [step("check", f"{CALL} check {BIN}/agent-plugin-{NAME}")]
    for what, engine in approvals.items():
        script.append(step(what, f'{CALL} job {BIN}/agent-plugin-{engine} {DEMO} "$(cat /tmp/look)"'))
    reports = " ".join(f"/tmp/{what}.out" for what in approvals)
    script.append(step("holds", f"{CALL} holds {BIN}/agent-plugin-{NAME} {reports}"))
    host.succeed(f"printf %s {shlex.quote(chr(10).join(script) + chr(10))} >/tmp/calls")
    host.succeed("chown dev /tmp/look /tmp/calls")

    dev("proj run demo call")
    host.wait_until_succeeds("test -e /tmp/calls.exit", timeout=LONG)

    # Every step's output first, so a failure shows all of them.
    done = {}
    for what, engine in [("check", NAME), *approvals.items(), ("holds", NAME)]:
        code = host.succeed(f"cat /tmp/{what}.exit").strip()
        out, err = host.succeed(f"cat /tmp/{what}.out"), host.succeed(f"cat /tmp/{what}.err")
        print(f"{what}: exit {code}\n{out}\n{err}")
        if code != "0" and what != "holds":
            print(host.execute(f"journalctl --no-pager -t agent-plugin-{engine} | tail -n 60")[1])
        done[what] = (code, err)

    with subtest("cn plugin check passes on the plugin, in its sandbox"):
        assert done["check"][0] == "0", done["check"][1]

    with subtest("the fixture runs under the plugin's describe, as the plugin"):
        for what in approvals:
            assert done[what][0] == "0", done[what][1]

    with subtest("what the plugin declares works under its grant, and nothing else"):
        assert done["holds"][0] == "0", done["holds"][1]
  '';
}
