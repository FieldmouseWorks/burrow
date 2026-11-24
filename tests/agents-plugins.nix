# Engine plugins in the sandbox, for real (docs/engines.md, "A plugin's
# sandbox"): a host VM runs the example plugin (crates/burrow-example-plugin)
# through Conary's own runner, from inside a proj job so the host's services
# are the real ones, and checks both halves of each grant: it works when
# declared and approved, and is absent, to the VM's root too, when it isn't.
#
#   nix build .#checks.x86_64-linux.agents -L
{ self, pkgs }:
let
  example = self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-example-plugin;
  plugin = example.overrideAttrs (old: {
    meta = (old.meta or { }) // { mainProgram = "burrow-example-plugin"; };
  });
  brings = pkgs.runCommand "example-plugin-brings" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "example-plugin-brings";
  } ''
    makeBinaryWrapper ${pkgs.lib.getExe plugin} $out/bin/example-plugin-brings \
      --set EXAMPLE_PLUGIN_BRINGS_SANDBOX 1
  '';
  # In no guest's closure: a path of the host's store, and only that.
  hostOnly = pkgs.writeText "host-only" "the host's alone\n";
  # A path each tool grant brings, to look for when it isn't granted.
  tools = {
    system = "${pkgs.ripgrep}/bin/rg";
    toolchains = "${pkgs.rustup}/bin/rustup";
  };
  inherit (import ./agents/capabilities.nix) grantable withheld;
in
pkgs.testers.runNixOSTest {
  name = "agents-plugins";

  nodes.host = { pkgs, config, ... }: let
    engine = name: port: capabilities: {
      inherit plugin capabilities;
      credentialFile = "/run/keys/${name}-token";
      credentialEnv = "EXAMPLE_TOKEN";
      hosts = [ "${name}.example" ];
      proxyPort = port;
    };
  in {
    imports = [ (import ./agents/plugin-host.nix { inherit self; }) ];
    burrow = {
      mcpServers.signed-in = {
        inherit (config.burrow.mcpServers.notes) command;
        actsAsOwner = true;
      };
      agents.engines.full = engine "full" 3131 (grantable ++ withheld) // {
        mcpServers = [ "notes" "signed-in" ];
        # Its VM's network comes up a minute and a half late, as a nested
        # boot's did on a loaded machine (#1444): the call still answers,
        # since only its deadline bounds the guest's waits.
        guestModules = [
          ({ pkgs, ... }: {
            systemd.services.systemd-networkd.serviceConfig = {
              ExecStartPre = [ "${pkgs.coreutils}/bin/sleep 90" ];
              TimeoutStartSec = "infinity";
            };
          })
        ];
      };
      agents.engines.files = engine "files" 3132 [ "workspace" ];
      agents.engines.bare = engine "bare" 3133 [ ];
      agents.engines.brings = engine "brings" 3134 grantable // { plugin = brings; };
    };

    environment.etc."host-only".source = hostOnly;
    networking.hosts."127.0.0.1" = [ "full.example" "bare.example" "forbidden.example" ];

    # What's there to be reached beside the proxy and an engine's host
    # (./agents/plugin-host.nix): the host itself.
    systemd.services = pkgs.lib.mapAttrs (_: listen: {
      wantedBy = [ "multi-user.target" ];
      path = [ pkgs.socat pkgs.coreutils pkgs.bash ];
      serviceConfig.Restart = "on-failure";
      script = "exec socat ${listen}";
    }) {
      reach-udp = "UDP4-RECVFROM:5353,fork EXEC:cat";
      reach-vsock = "VSOCK-LISTEN:9999,fork SYSTEM:'echo host-vsock'";
    };
  };

  testScript = ''
    import json, re, shlex

    GRANTABLE = ${builtins.toJSON grantable}
    TOOLS = ${builtins.toJSON tools}
    HOST_ONLY = "${hostOnly}"
    DEMO = "/data/dev/src/demo"
    OTHER = "/data/dev/src/other"
    BIN = "/run/current-system/sw/bin"
    CALL = f"{BIN}/burrow-example-plugin-call"
    # As long as several nested boots may take on a loaded machine.
    LONG = 3600

    SECRETS = [
        "full-engine-secret", "files-engine-secret", "bare-engine-secret",
        "host-key-secret", "host-gh-login", "caller-gh-token",
        "repo-config-token", "remote-url-token", "other-project-secret",
    ]

    host.wait_for_unit("multi-user.target")
    host.wait_for_unit("agents-dataset.service")
    for engine in ["full", "files", "bare", "brings"]:
        host.wait_for_unit(f"agent-proxy-{engine}.service")
        host.wait_for_unit(f"agent-plugin-{engine}.socket")
    for unit in ["reach-tcp", "reach-udp", "reach-vsock"]:
        host.wait_for_unit(f"{unit}.service")

    def dev(cmd):
        return host.succeed(f"su - dev -c {shlex.quote(cmd)}")

    def put(path, text):
        host.succeed(f"printf %s {shlex.quote(text)} > {path}")

    host.succeed("mkdir -p /run/keys")
    for engine in ["full", "files", "bare", "brings"]:
        put(f"/run/keys/{engine}-token", f"{engine}-engine-secret")
    put("/run/keys/host-token", "host-key-secret")
    dev("mkdir -p ~/.config/gh && echo host-gh-login >~/.config/gh/hosts.yml")
    host.succeed("mkdir -p /var/lib/burrow/settings && echo untouched >/var/lib/burrow/settings/sentinel")

    git = "git -c user.name=Test -c user.email=test@example.com"
    dev(f"mkdir {DEMO} && cd {DEMO} && git init -q -b main && echo one >a && git add a && {git} commit -qm one"
        " && git config http.https://github.com/.extraheader 'AUTHORIZATION: repo-config-token'"
        " && git remote add origin https://user:remote-url-token@example.com/o/demo")
    dev(f"mkdir {OTHER} && cd {OTHER} && git init -q -b main && echo other-project-secret >secret && git add secret && {git} commit -qm other")
    other_commit = dev(f"git -C {OTHER} rev-parse HEAD").strip()

    def host_state():
        # What a plugin could change only by changing settings or installing.
        return host.succeed(
            "readlink /run/current-system; ls -lA /var/lib/burrow/settings; cat /var/lib/burrow/settings/sentinel;"
            " ls /etc/systemd/system; ls /run/agent-plugin"
        )

    before = host_state()

    around = [
        "10.0.2.2:443", "10.0.2.2:3128", "10.0.2.2:3129", "10.0.2.2:3131", "10.0.2.2:22",
        "192.168.1.1:443", "10.0.2.100:443", "10.0.2.100:3129",
    ]

    def look(**more):
        base = {
            "allowed": ["full.example", "engine-reached"],
            "hosts": ["forbidden.example", "bare.example", "127.0.0.1"],
            "tcp": list(around),
            "udp": ["10.0.2.2:5353", "10.0.2.3:53", "192.168.1.1:5353"],
            "vsock": [9999],
            "secrets": SECRETS,
            "read": [
                f"{DEMO}/a", f"{DEMO}/.git/config", f"{OTHER}/secret", HOST_ONLY, *TOOLS.values(),
                "/run/keys/host-token", "/run/keys/bare-token", "/var/lib/burrow/settings/sentinel",
            ],
            "write": [
                "/nix/store/plugin-wrote", "/nix/.ro-store/plugin-wrote-too",
                "/var/lib/burrow/settings/overlay.json", "/etc/systemd/system/plugin-installed.service",
                f"{OTHER}/planted",
            ],
            "tags": ["share0", "share1", "home", "data"],
            "asks": [
                ["settings", "pause", "o/demo"], ["engines", "use", "another"],
                ["run", "demo", "install a plugin"],
                # Into a host job, which has the authority the plugin hasn't.
                ["reply", "bystander", "change the settings"], ["stop", "bystander"],
                ["withdraw", "bystander"],
            ],
        }
        return {**base, **more}

    calls = {
        "full": look(),
        "files": look(write=[f"{DEMO}/.git/planted"]),
        "bare": look(
            tcp=around + [f"10.0.2.101:{port}" for port in range(7000, 7005)] + ["10.0.2.100:3128"],
            tags=["share0", "share1", "home", "data", "workspace"],
        ),
        "brings": look(),
    }
    script = []
    for engine, asked in calls.items():
        put(f"/tmp/{engine}.look", json.dumps(asked))
        script.append(
            f'{CALL} job {BIN}/agent-plugin-{engine} {DEMO} "$(cat /tmp/{engine}.look)"'
            f" >/tmp/{engine}.out 2>/tmp/{engine}.err; echo $? >/tmp/{engine}.exit"
        )
    script.append(f"{CALL} check {BIN}/agent-plugin-full >/tmp/check.out 2>/tmp/check.err; echo $? >/tmp/check.exit")
    put("/tmp/calls", "\n".join(script) + "\n")
    host.succeed("chown dev /tmp/*.look /tmp/calls")

    job = dev("proj run demo call").strip().splitlines()[-1]
    host.wait_until_succeeds("test -e /tmp/calls.exit", timeout=LONG)

    def called(engine):
        err = host.succeed(f"cat /tmp/{engine}.err")
        print(f"{engine}: {err}")
        return host.succeed(f"cat /tmp/{engine}.exit").strip(), host.succeed(f"cat /tmp/{engine}.out"), err

    reports, logs = {}, {}
    for engine in ["full", "files", "bare"]:
        code, out, err = called(engine)
        if code != "0":
            print(host.execute("journalctl --no-pager -t agent-plugin-" + engine + " | tail -n 200")[1])
            raise Exception(f"{engine}'s call failed: {err}")
        reports[engine], logs[engine] = json.loads(out), err
        print(engine, json.dumps(reports[engine], indent=1))

    def working(engine):
        return sorted(name for name, ok in reports[engine]["works"].items() if ok)

    full, files, bare = reports["full"], reports["files"], reports["bare"]

    with subtest("each approved capability works, and the plugin is told of exactly those"):
        assert working("full") == sorted(GRANTABLE), full["said"]
        assert sorted(full["told"]) == sorted(GRANTABLE), full["told"]
        assert working("files") == ["workspace"], files["said"]
        assert files["told"] == ["workspace"], files["told"]
        assert full["said"]["heavy"].strip().isdigit(), full["said"]["heavy"]

    with subtest("nothing that wasn't approved works, for the VM's root either"):
        assert working("bare") == [], bare["said"]
        assert bare["told"] == [], bare["told"]
        assert bare["mounted"] == [], bare["mounted"]
        assert bare["readable"] == [], bare["readable"]
        assert files["readable"] == [f"{DEMO}/a"], files["readable"]

    with subtest("it reads its own engine's login, and no other engine's or the host's"):
        # Finding its own is what shows the search would find another's.
        for engine, report in reports.items():
            assert report["found"] == [f"{engine}-engine-secret"], (engine, report["found"])
            assert "EXAMPLE_TOKEN" in report["env"], (engine, report["env"])
            for name in ["GH_TOKEN", "GITHUB_TOKEN", "GH_TOKEN_SOCKET", "PROJ_JOB", "PROJ_CONFIG"]:
                assert name not in report["env"], (engine, name)

    with subtest("no path it wasn't granted is there to read or write"):
        expected = [f"{DEMO}/a", f"{DEMO}/.git/config", *TOOLS.values()]
        assert sorted(full["readable"]) == sorted(expected), full["readable"]
        assert full["mounted"] == [], full["mounted"]
        for engine in ["full", "bare"]:
            for path in reports[engine]["written"]:
                # What it wrote elsewhere was its VM's own.
                assert "plugin-wrote" not in path, (engine, path)
        assert files["written"] == [], files["written"]
        host.fail(f"test -e {OTHER}/planted")
        host.fail("test -e /nix/store/plugin-wrote")
        host.fail(f"test -e {DEMO}/.git/planted")
        host.fail(f"test -e {DEMO}/plugin-was-here")

    with subtest("egress: its engine's allowlist through the proxy, and no way around it"):
        assert "engine-reached" in full["said"]["network"], full["said"]
        for name, answer in full["proxied"].items():
            assert " 403 " in answer, (name, answer)
            assert "engine-reached" not in answer, (name, answer)
        for engine, report in reports.items():
            assert report["dialed"] == [], (engine, report["dialed"])
        for engine in ["files", "bare"]:
            for name, answer in reports[engine]["proxied"].items():
                assert answer.startswith("no proxy"), (engine, name, answer)

    with subtest("the host's services answer it as the user, with nothing of the caller's"):
        assert "total" in full["said"]["probe"], full["said"]["probe"]
        assert full["said"]["mcp"].strip() == "dev registry-setting no-token ping", full["said"]["mcp"]

    with subtest("it can't change settings or install a plugin"):
        assert len(full["asked"]) == 6, full["asked"]
        for answer in full["asked"]:
            assert "job-state only reads" in answer, answer
        assert host_state() == before
        host.fail("test -e /etc/systemd/system/plugin-installed.service")
        for name in ["github", "environment", "github-token", "settings-write", "plugin-install", "mcp:signed-in"]:
            assert f'"{name}" refused' in logs["full"], (name, logs["full"])

    with subtest("its work comes back as refs, and its clone goes"):
        for engine in ["full", "files"]:
            found = re.search(r"refs/agents/([a-z0-9-]+)/", logs[engine])
            assert found, logs[engine]
            at = found.group(1)
            assert dev(f"git -C {DEMO} show refs/agents/{at}/worktree:plugin-was-here").strip() == "the plugin's"
            host.wait_until_succeeds(f"! zfs list rpool/agents/{at}", timeout=120)
        assert "refs/agents" not in logs["bare"], logs["bare"]
        # The clone's config was cleaned, and the project's own is as it was.
        assert "repo-config-token" in host.succeed(f"cat {DEMO}/.git/config")

    with subtest("a plugin that brings a sandbox of its own gets no request run"):
        code, out, err = called("brings")
        assert code != "0" and "brings a sandbox of its own" in err, (code, err)
        assert out == "", out

    with subtest("Conary's conformance check passes on the executable as it is"):
        code, out, err = called("check")
        assert code == "0" and json.loads(out) == ["job"], (code, out, err)

    with subtest("a plugin's door is its user's alone"):
        host.fail("runuser -u microvm -- socat -u OPEN:/dev/null UNIX-CONNECT:/run/agent-plugin/full.sock")
        host.succeed("runuser -u dev -- socat -u OPEN:/dev/null UNIX-CONNECT:/run/agent-plugin/full.sock")

    def no_call_left():
        host.wait_until_succeeds("! systemctl list-units --plain --no-legend --state=active,activating,deactivating 'agent-plugin-*@*.service' | grep .", timeout=120)
        host.fail("pgrep -u microvm")

    with subtest("no call leaves anything running or failed"):
        no_call_left()
        host.fail("systemctl --failed --plain --no-legend | grep agent-plugin")

    with subtest("a caller that's killed ends its call, and the work still comes back"):
        refs = f"git -C {DEMO} for-each-ref --format='%(refname)' refs/agents/"
        had = set(dev(refs).split())
        put("/tmp/wait.look", json.dumps({"sleep": 3000}))
        host.succeed(
            f"systemd-run --unit=caller --uid=dev --property=WorkingDirectory=/tmp {CALL} job"
            f" {BIN}/agent-plugin-full {DEMO} \"$(cat /tmp/wait.look)\""
        )
        host.wait_until_succeeds("journalctl --no-pager -u caller | grep -q 'example plugin: waiting'", timeout=LONG)
        host.succeed("systemctl kill -s KILL caller.service")
        no_call_left()
        new = [ref for ref in set(dev(refs).split()) - had if ref.endswith("/worktree")]
        assert len(new) == 1, new
        assert dev(f"git -C {DEMO} show {new[0]}:plugin-was-here").strip() == "the plugin's"
        host.fail(f"zfs list rpool/agents/{new[0].split('/')[2]}")

    with subtest("links it plants in its repository bring back nothing of another"):
        host.succeed("systemctl reset-failed")
        planted = {
            ".git/objects/info/alternates": f"{OTHER}/.git/objects\n",
            ".git/refs/heads/stolen": f"{other_commit}\n",
        }
        put("/tmp/thief.look", json.dumps({"plant": planted}))
        host.succeed("chown dev /tmp/thief.look")
        status, said = host.execute(
            "su - dev -c " + shlex.quote(f'{CALL} job {BIN}/agent-plugin-full {DEMO} "$(cat /tmp/thief.look)"') + " 2>&1"
        )
        print(said)
        assert status != 0 and "couldn't be saved" in said, (status, said)
        host.fail(f"su - dev -c 'git -C {DEMO} cat-file -e {other_commit}'")
        # It's kept, as a sandbox with no VM to start.
        found = re.search(r"agent fetch (demo-[0-9]+)", said)
        assert found, said
        kept = found.group(1)
        assert kept in dev("agent ls")
        # Keeping it is no failure: a failed unit would be a job for the
        # machine, whenever a plugin liked.
        no_call_left()
        host.fail("systemctl --failed --plain --no-legend | grep agent-plugin")
        host.fail(f"find {DEMO}/.git -name '*.lock' | grep .")
        host.fail(f"su - dev -c 'agent sh {kept} true'")
        host.fail(f"su - dev -c 'git -C {DEMO} cat-file -e {other_commit}'")
        # And that's the confinement's doing: the same save, unconfined,
        # follows the link.
        host.execute(f"su - dev -c 'agent _save {kept}'")
        host.succeed(f"su - dev -c 'git -C {DEMO} cat-file -e {other_commit}'")
  '';
}
