# End-to-end integration test on a generic NixOS machine for #746:
# Install a pinned third conformance plugin through the authenticated GitHub
# settings door (with real bubblewrap-sandboxed `cn plugin check`), verify
# immediate role and per-job schema/picker choices, run a real `proj` job with
# a declared model, update, verify failed conformance preserves the prior active
# installation, prove non-owner, job-originated, and tampered-artifact cases
# fail (before install and at subsequent start), and verify atomic removal
# fallback and audit log records.
{ self, pkgs }:
let
  system = pkgs.stdenv.hostPlatform.system;
  examplePlugin = self.packages.${system}.burrow-example-plugin;

  # Third conformance plugin v1 (Nix-built package in /nix/store).
  thirdV1 = pkgs.runCommand "third-conformance-plugin-1.0.0" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "third";
  } ''
    makeBinaryWrapper ${pkgs.lib.getExe' examplePlugin "burrow-example-plugin"} $out/bin/third \
      --set EXAMPLE_PLUGIN_ENGINE third \
      --set EXAMPLE_PLUGIN_SANDBOX none \
      --set EXAMPLE_PLUGIN_DECLARES network \
      --set EXAMPLE_PLUGIN_ECHO_JOB 1 \
      --set EXAMPLE_PLUGIN_MODELS '[{"id":"third-model-v1","efforts":["low","high"]}]'
  '';

  # Third conformance plugin v2 (used as a verified release artifact).
  thirdV2 = pkgs.runCommand "third-conformance-plugin-2.0.0" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "third";
  } ''
    makeBinaryWrapper ${pkgs.lib.getExe' examplePlugin "burrow-example-plugin"} $out/bin/third \
      --set EXAMPLE_PLUGIN_ENGINE third \
      --set EXAMPLE_PLUGIN_SANDBOX none \
      --set EXAMPLE_PLUGIN_DECLARES network \
      --set EXAMPLE_PLUGIN_ECHO_JOB 1 \
      --set EXAMPLE_PLUGIN_MODELS '[{"id":"third-model-v1","efforts":["low","high"]},{"id":"third-model-v2","efforts":["medium","max"]}]'
  '';

  # Non-conforming plugin that fails `cn plugin check`.
  badConformance = pkgs.writeShellScript "third-bad-conformance" ''
    IFS= read -r _ || true
    printf '%s\n' '{"contract":"conary-plugin v1","by":{"engine":"wrong","session":null},"kind":"describe","versions":["conary-plugin v5"],"kinds":[],"v2_kinds":["job"],"v4_kinds":[],"models":[],"capabilities":{"sandbox":"none","network":false,"devices":[],"mcp":false},"meters":[]}'
  '';

  # Fake GitHub CLI driven by /var/lib/settings-fixture/issue.json.
  gh = pkgs.writeShellScriptBin "gh" ''
    printf 'HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n'
    case "$*" in
      *graphql*)
        cat >/dev/null
        if [ -f /var/lib/settings-fixture/graphql.json ]; then
          cat /var/lib/settings-fixture/graphql.json
        else
          printf '{}'
        fi
        ;;
      *comments*)
        cat >> /var/lib/settings-fixture/comments.log
        printf '\n' >> /var/lib/settings-fixture/comments.log
        printf '{}'
        ;;
      *issues/*)
        cat >/dev/null
        printf '{}'
        ;;
      *issues*)
        if [ -f /var/lib/settings-fixture/issues.json ]; then
          cat /var/lib/settings-fixture/issues.json
        else
          printf '[]'
        fi
        ;;
      *)
        printf '[]'
        ;;
    esac
  '';
in
pkgs.testers.runNixOSTest {
  name = "settings-plugins";
  nodes.machine = { lib, ... }: {
    imports = [
      self.nixosModules.proj
      ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs = { }; fileSystems = { }; boot = { }; })
    ];
    burrow.claude.package = pkgs.hello;
    burrow.proj = {
      enable = true;
      minFreeMemory = 0;
      inherit gh;
    };
    users.users.dev.linger = true;
    burrow.settings = {
      allowed = [ "sam/demo" ];
      owners = [ { login = "sam"; id = 1; } ];
      plugins.fallback = "claude";
      door = {
        enable = true;
        repo = "sam/box";
        authors = [ { login = "sam"; id = 1; } ];
        inherit gh;
      };
    };
    environment.systemPackages = [
      pkgs.jq
      pkgs.check-jsonschema
      pkgs.git
    ];
    systemd.tmpfiles.rules = [
      "d /var/lib/settings-fixture 0777 dev dev -"
      "d /data/dev/src 0755 dev dev -"
      "d /data/dev/jobs 0755 dev dev -"
      "d /data/dev/scratch 0755 dev dev -"
    ];
  };

  testScript = ''
    import json, shlex

    V1_BIN = "${thirdV1}/bin/third"
    V2_SRC = "${thirdV2}/bin/third"
    BAD_BIN = "${badConformance}"

    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("user@1001.service")
    machine.wait_for_unit("proj-settings-migrate.service")
    machine.succeed("systemctl stop burrow-settings-door.timer")
    machine.succeed("su - dev -c 'mkdir -p /data/dev/src/demo && cd /data/dev/src/demo && git init -q -b main'")

    def sha256(path):
        return "sha256:" + machine.succeed(f"sha256sum {shlex.quote(path)}").split()[0]

    def door_ask(num, opener_login, opener_id, body):
        issues = [{"number": num, "user": {"login": opener_login, "id": opener_id}}]
        gql = {
            "data": {
                "repository": {
                    "issue": {
                        "id": f"issue-{num}",
                        "author": {"login": opener_login, "databaseId": opener_id},
                        "title": "settings",
                        "body": body,
                        "openCycle": {"nodes": []},
                        "userContentEdits": {"totalCount": 0, "nodes": []},
                        "timelineItems": {"totalCount": 0, "nodes": []},
                    }
                }
            }
        }
        machine.succeed(f"printf %s {shlex.quote(json.dumps(issues))} >/var/lib/settings-fixture/issues.json")
        machine.succeed(f"printf %s {shlex.quote(json.dumps(gql))} >/var/lib/settings-fixture/graphql.json")
        machine.succeed("rm -f /var/lib/settings-fixture/comments.log")
        machine.succeed("systemctl start burrow-settings-door.service")
        machine.succeed("rm -f /var/lib/settings-fixture/issues.json /var/lib/settings-fixture/graphql.json")
        status, out = machine.execute("cat /var/lib/settings-fixture/comments.log 2>/dev/null || true")
        return out

    v1_hash = sha256(V1_BIN)
    machine.succeed(f"cp {shlex.quote(V2_SRC)} /var/lib/settings-fixture/third-v2 && chmod 0755 /var/lib/settings-fixture/third-v2")
    v2_hash = sha256("/var/lib/settings-fixture/third-v2")
    bad_hash = sha256(BAD_BIN)

    with subtest("non-owner account id and job-originated installs fail"):
        # Same login "sam" with wrong account ID 999 must be refused.
        door_ask(1, "sam", 999, f"plugin install third nix {V1_BIN} 1.0.0 {v1_hash}")
        assert "third" not in machine.succeed("runuser -u dev -- settings plugin list")
        # Job-originated CLI calls fail closed.
        machine.fail(f"runuser -u dev -- env PROJ_JOB=j1 settings plugin install third nix {V1_BIN} 1.0.0 {v1_hash}")
        machine.fail(f"runuser -u dev -- env PROJ_JOB=j1 proj settings plugin install third nix {V1_BIN} 1.0.0 {v1_hash}")

    with subtest("tampered artifact fails before installation and never executes"):
        canary = "/var/lib/settings-fixture/preinstall.canary"
        tampered = "/var/lib/settings-fixture/tampered-plugin"
        machine.succeed(f"printf '#!/bin/sh\\ntouch {canary}\\n' > {tampered} && chmod 0755 {tampered}")
        reply = door_ask(2, "sam", 1, f"plugin install third release {tampered} 1.0.0 sha256:{'0'*64}")
        assert "Nothing changed:" in reply and "hash mismatch" in reply, reply
        machine.fail(f"test -e {canary}")
        assert "third" not in machine.succeed("runuser -u dev -- settings plugin list")

    with subtest("authenticated owner installs pinned Nix third conformance plugin with sandboxed cn plugin check"):
        reply = door_ask(3, "sam", 1, f"plugin install third nix {V1_BIN} 1.0.0 {v1_hash}")
        assert "Installed engine plugin third 1.0.0" in reply, reply
        assert "third-model-v1" in reply, reply

    with subtest("immediate schema choices and picker data include declared models, efforts, and capabilities"):
        machine.succeed("runuser -u dev -- proj settings schema >/tmp/schema.json")
        machine.succeed("runuser -u dev -- proj settings show --json >/tmp/show.json")
        machine.succeed("check-jsonschema --schemafile /tmp/schema.json /tmp/show.json")
        show = json.loads(machine.succeed("cat /tmp/show.json"))
        assert show["options"]["proj.enginePlugins"]["value"]["third"]["version"] == "1.0.0"
        assert show["options"]["proj.enginePlugins"]["value"]["third"]["sha256"] == v1_hash
        assert show["options"]["proj.engineOffers"]["value"]["third"]["models"] == ["third-model-v1"]
        schema = json.loads(machine.succeed("cat /tmp/schema.json"))
        job_choices = schema["$defs"]["jobChoice"]["oneOf"]
        third_choice = [c for c in job_choices if c["properties"]["engine"]["const"] == "third"][0]
        assert third_choice["properties"]["model"]["anyOf"][1]["enum"] == ["third-model-v1"]
        assert third_choice["x-capabilities"]["sandbox"] == "none"
        picker = json.loads(machine.succeed("runuser -u dev -- proj settings picker"))
        assert picker["engines"]["third"]["version"] == "1.0.0"
        assert picker["engines"]["third"]["capabilities"]["network"] is True

    with subtest("run a proj job on the installed plugin with its declared model"):
        job_id = machine.succeed(
            "su - dev -c 'proj run --local-work --engine third --model third-model-v1 --effort high demo \"hello third\"'"
        ).strip()
        machine.wait_until_succeeds(
            f"test \"$(su - dev -c 'proj state {shlex.quote(job_id)}')\" = done",
            timeout=60,
        )
        answer = machine.succeed(f"su - dev -c 'proj log {shlex.quote(job_id)}'")
        assert "engine=third model=third-model-v1 effort=high prompt=" in answer, answer
        assert machine.succeed(f"cat /data/dev/jobs/{job_id}/1.model").strip() == "third-model-v1"
        assert machine.succeed(f"cat /data/dev/jobs/{job_id}/1.effort").strip() == "high"
        # Undeclared model is rejected up front.
        machine.fail("su - dev -c 'proj run --local-work --engine third --model bogus demo \"nope\"'")

    with subtest("failed conformance update preserves prior active installation"):
        reply = door_ask(4, "sam", 1, f"plugin update third release {BAD_BIN} 2.0.0-bad {bad_hash}")
        assert "Nothing changed:" in reply and "cn plugin check" in reply, reply
        info = json.loads(machine.succeed("runuser -u dev -- settings plugin show third"))
        assert info["version"] == "1.0.0"
        assert info["sha256"] == v1_hash

    with subtest("valid release artifact update activates v2 and tampered artifact fails at subsequent start"):
        reply = door_ask(5, "sam", 1, f"plugin update third release /var/lib/settings-fixture/third-v2 2.0.0 {v2_hash}")
        assert "Updated engine plugin third 2.0.0" in reply, reply
        assert "third-model-v2" in reply, reply
        job2 = machine.succeed(
            "su - dev -c 'proj run --local-work --engine third --model third-model-v2 --effort max demo \"hello v2\"'"
        ).strip()
        machine.wait_until_succeeds(
            f"test \"$(su - dev -c 'proj state {shlex.quote(job2)}')\" = done",
            timeout=60,
        )
        ans2 = machine.succeed(f"su - dev -c 'proj log {shlex.quote(job2)}'")
        assert "engine=third model=third-model-v2 effort=max" in ans2, ans2

        # Tamper with the stored release artifact on disk and prove start fails without executing it.
        installed_path = json.loads(machine.succeed("runuser -u dev -- settings plugin show third"))["path"]
        saved_copy = "/var/lib/settings-fixture/saved-v2"
        start_canary = "/var/lib/settings-fixture/start.canary"
        machine.succeed(f"cp {shlex.quote(installed_path)} {saved_copy}")
        machine.succeed(f"chmod 0755 {shlex.quote(installed_path)} && printf '#!/bin/sh\\ntouch {start_canary}\\n' > {shlex.quote(installed_path)}")
        machine.fail("runuser -u dev -- proj check-config")
        machine.fail("runuser -u dev -- settings plugin verify third")
        job_bad = machine.succeed(
            "su - dev -c 'proj run --local-work --engine third --model third-model-v2 demo \"tampered\"'"
        ).strip()
        machine.wait_until_succeeds(
            f"test \"$(su - dev -c 'proj state {shlex.quote(job_bad)}')\" = failed",
            timeout=60,
        )
        machine.fail(f"test -e {start_canary}")
        # Restore valid artifact bytes before removal test.
        machine.succeed(f"cp {saved_copy} {shlex.quote(installed_path)} && chmod 0555 {shlex.quote(installed_path)}")
        machine.succeed("runuser -u dev -- settings plugin verify third")

    with subtest("removal refuses invalid fallback, moves affected roles atomically on valid fallback, and audits all mutations"):
        machine.succeed("runuser -u dev -- proj settings role work third")
        machine.succeed("runuser -u dev -- proj settings model work third third-model-v2 max")
        bad_rm = door_ask(6, "sam", 1, "plugin remove third --fallback ghost")
        assert "Nothing changed:" in bad_rm, bad_rm
        assert "third" in machine.succeed("runuser -u dev -- settings plugin list")

        rm_reply = door_ask(7, "sam", 1, "plugin remove third")
        assert "Removed engine plugin third 2.0.0" in rm_reply, rm_reply
        assert "Fallback engine: claude" in rm_reply, rm_reply
        assert "- work: third -> claude" in rm_reply, rm_reply

        show_after = json.loads(machine.succeed("runuser -u dev -- proj settings show --json"))
        assert show_after["options"]["proj.enginePlugins"]["value"] == {}
        assert show_after["options"]["proj.roles"]["value"]["work"]["engine"] == "claude"

        # Verify audit records in overlay.log contain actor, source, version, hash.
        log_text = machine.succeed("cat /var/lib/burrow/settings/overlay.log")
        records = [json.loads(line) for line in log_text.splitlines() if line.strip()]
        plug_recs = [r for r in records if "plugin" in r]
        assert len(plug_recs) == 3, plug_recs
        ops = [r["plugin"]["operation"] for r in plug_recs]
        assert ops == ["install", "update", "remove"], ops
        for rec in plug_recs:
            assert rec["actorId"] == 1
            assert "github:sam/box#" in rec["actor"]
            assert rec["plugin"]["source"]
            assert rec["plugin"]["version"]
            assert rec["plugin"]["sha256"].startswith("sha256:")
        assert plug_recs[1]["plugin"]["previousVersion"] == "1.0.0"
        assert plug_recs[1]["plugin"]["previousSha256"] == v1_hash
        assert plug_recs[2]["plugin"]["fallback"] == "claude"
        assert plug_recs[2]["plugin"]["movedRoles"] == [{"role": "work", "from": "third", "to": "claude"}]
  '';
}
