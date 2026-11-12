# The off switch works with the rest of the pipeline down. Only the settings
# door runs: it files a pause for a repo the flake allows, then the inbox
# starts again and takes the issue of a repo still allowed but not the paused
# one's. The pause exists only in the runtime overlay, so an inbox that reads
# the flake's allow list instead of the overlay takes both issues and fails
# here. `probe settings` shows the same policy.
{ self, pkgs }:
let
  inherit (import ./proj/stand-ins.nix { inherit pkgs; filterLabels = true; }) gh;
  doorGh = pkgs.writeShellScriptBin "gh" ''
    if [ -e /var/lib/settings-fixture/limited ]; then
      printf 'HTTP/2.0 429 Too Many Requests\r\nRetry-After: 3\r\nContent-Type: application/json\r\n\r\n{"message":"limited"}'
      exit 1
    fi
    printf 'HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n'
    case "$*" in
      *graphql*)
        cat >/dev/null
        if [ -e /var/lib/settings-fixture/pause ]; then
          cat <<'JSON'
{"data":{"repository":{"issue":{"id":"one","author":{"login":"sam","databaseId":1},"title":"settings","body":"pause sam/code","openCycle":{"nodes":[]},"userContentEdits":{"totalCount":0,"nodes":[]},"timelineItems":{"totalCount":0,"nodes":[]}}}}}
JSON
        else
          printf '{}'
        fi
        ;;
      *comments*) cat >/dev/null; printf '{}' ;;
      *issues/1*) cat >/dev/null; printf '{}' ;;
      *issues*)
        if [ -e /var/lib/settings-fixture/pause ]; then
          printf '[{"number":1,"user":{"login":"sam","id":1}}]'
        else printf '[]'; fi
        ;;
      *) printf '[]' ;;
    esac
  '';
in pkgs.testers.runNixOSTest {
  name = "settings-off-switch";
  nodes.machine = { config, lib, ... }: {
    imports = [ self.nixosModules.autoland self.nixosModules.updates self.nixosModules.probe ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs = { }; fileSystems = { }; boot = { }; }) ];
    burrow.claude.package = pkgs.hello;
    burrow.events = { host = "example.org"; secretFile = "/run/secrets/webhook"; };
    burrow.autoland = {
      enable = true;
      owners = [ "sam" ];
      authors = [ "sam" ];
      app = { id = 123456; accountId = 123457; slug = "sams-steward"; keyFile = "/run/secrets/steward"; };
      reviewers.codex.enable = true;
      pipelineRepos = [ "sam/box" ];
    };
    burrow.ledger = {
      enable = true;
      package = pkgs.writeShellScriptBin "cn" "exit 0";
      anchorRepo = "sam/box";
    };
    burrow.updates = { enable = true; origin = "https://github.com/sam/box"; };
    burrow.proj = {
      enable = true;
      inherit gh;
      inbox = { enable = true; authors = [ { login = "sam"; id = 1; } ]; };
    };
    burrow.settings = {
      allowed = [ "sam/code" "sam/web" "sam/box" ];
      door = { enable = true; repo = "sam/box"; authors = [ { login = "sam"; id = 1; } ]; gh = doorGh; };
    };
    burrow.probe = { enable = true; repo = "sam/box"; authors = [ { login = "sam"; id = 1; } ]; gh = doorGh; };
    environment.etc."settings-tracked".source = config.burrow.settings.file;
    environment.systemPackages = [ pkgs.git pkgs.jq self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-settings ];
    systemd.tmpfiles.rules = [
      "d /var/lib/settings-fixture 0755 dev dev -"
      "d /data/dev/src/code 0755 dev dev -"
      "d /data/dev/src/web 0755 dev dev -"
    ];
  };
  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    pipeline = "proj-retry* burrow-events.service proj-inbox* proj-watch* autoland* burrow-update* burrow-ledger* burrow-watchdog* remi-update*"
    machine.succeed("systemctl mask --runtime proj-retry-transfer.service proj-retry-barrier.service burrow-events.service proj-inbox.service proj-inbox@.service proj-watch.service proj-watch@.service autoland.service autoland@.service autoland-review.service burrow-update.service burrow-ledger.service burrow-ledger@.service burrow-watchdog.service remi-update.service")
    stopped, output = machine.execute("systemctl stop " + " ".join(shlex.quote(u) for u in pipeline.split()))
    if stopped:
        print(output)
    machine.wait_until_succeeds("test -z \"$(systemctl list-units --plain --no-legend --state=active,activating,deactivating " + pipeline + ")\"", timeout=30)
    def dev(cmd):
        return machine.succeed("runuser -u dev -- bash -lc " + shlex.quote(cmd)).strip()
    def data(endpoint, value):
        machine.succeed("mkdir -p /tmp/gh && printf %s " + shlex.quote(json.dumps(value)) + " >/tmp/gh/" + endpoint.replace("/", "_") + "; chmod -R a+rwX /tmp/gh")
    for name in ["code", "web"]:
        dev(f"git -C /data/dev/src/{name} init -q && git -C /data/dev/src/{name} -c user.name=F -c user.email=f@example.org commit -q --allow-empty -m initial && git -C /data/dev/src/{name} remote add origin https://github.com/sam/{name}")
        issue = {"number": 7, "user": {"login": "sam", "id": 1, "type": "User"}, "html_url": f"https://github.com/sam/{name}/issues/7", "repository_url": f"https://api.github.com/repos/sam/{name}", "title": "work", "body": "do it", "state": "open", "labels": [{"name": "claude"}], "created_at": "2026-01-01T00:00:00Z"}
        data(f"repos/sam/{name}/issues", [issue]); data(f"repos/sam/{name}/issues/7", issue); data(f"repos/sam/{name}/issues/7/comments", [])
    # Both are allowed by the flake; the probe says so.
    for name in ["code", "web"]:
        machine.succeed("burrow-settings /etc/settings-tracked allows sam/" + name)
    # The pause goes in through the door alone.
    machine.succeed("touch /var/lib/settings-fixture/pause")
    machine.succeed("systemctl start burrow-settings-door.service")
    machine.wait_until_fails("burrow-settings /etc/settings-tracked allows sam/code", timeout=90)
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/web")
    machine.succeed("test $(systemctl show -P Result burrow-settings-door.service) = success")
    answer = dev("burrow-probe ask settings")
    assert "paused: sam/code" in answer, answer
    # Now the inbox starts again: the paused repo's issue is left alone.
    machine.succeed("systemctl unmask --runtime proj-inbox.service proj-inbox@.service")
    dev("proj-inbox sam/web")
    dev("proj-inbox sam/code")
    machine.succeed("test -s /var/lib/burrow/proj-inbox/sam_web-7/job")
    machine.succeed("test ! -e /var/lib/burrow/proj-inbox/sam_code-7/job")
    machine.fail("grep -F 'sam/code/issues/7' /tmp/gh/calls")
    for unit in ["burrow-events", "autoland", "autoland-review", "burrow-update", "burrow-ledger", "proj-watch"]:
        machine.fail(f"systemctl is-active {unit}.service")
  '';
}
