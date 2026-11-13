# The off switch still works with the pipeline stopped. The next reader uses
# exactly the same Rust policy as inbox admission and the steward.
{ self, pkgs }:
let
  gh = pkgs.writeShellScriptBin "gh" ''
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
  name = "settings-door";
  nodes.machine = { config, lib, ... }: {
    imports = [ self.nixosModules.autoland self.nixosModules.updates ((import ../examples/machine.nix { inherit lib; }) // { nixpkgs = { }; fileSystems = { }; boot = { }; }) ];
    burrow.claude.package = pkgs.hello;
    burrow.authors = [ { login = "sam"; id = 1; } ];
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
      inbox.enable = true;
    };
    burrow.settings = {
      allowed = [ "sam/code" "sam/box" ];
      door = { enable = true; repo = "sam/box"; inherit gh; };
    };
    environment.etc."settings-tracked".source = config.burrow.settings.file;
    environment.systemPackages = [ self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-settings ];
    systemd.tmpfiles.rules = [ "d /var/lib/settings-fixture 0755 dev dev -" ];
  };
  testScript = ''
    machine.wait_for_unit("multi-user.target")
    # No scheduler or inbox is available to perform the change.
    machine.succeed('systemctl mask --runtime proj-retry-transfer.service proj-retry-barrier.service burrow-events.service proj-inbox.service proj-inbox@.service proj-watch.service proj-watch@.service autoland.service autoland@.service autoland-review.service burrow-update.service burrow-ledger.service burrow-ledger@.service')
    # Boot can cancel queued control jobs; require the actual stopped state.
    stopped, output = machine.execute("systemctl stop 'proj-retry*' 'burrow-events.service' 'proj-inbox*' 'proj-watch*' 'autoland*' 'burrow-update*' 'burrow-ledger*'")
    if stopped:
        print(output)
    machine.wait_until_succeeds('test -z "$(systemctl list-units --plain --no-legend --state=active,activating,deactivating \'proj-retry*\' \'burrow-events.service\' \'proj-inbox*\' \'proj-watch*\' \'autoland*\' \'burrow-update*\' \'burrow-ledger*\')"', timeout=30)
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/code")
    machine.fail("burrow-settings /etc/settings-tracked allows sam/new")
    machine.succeed("touch /var/lib/settings-fixture/pause /var/lib/settings-fixture/limited")
    machine.succeed("systemctl start burrow-settings-door.service")
    machine.succeed("test $(systemctl show -P Result burrow-settings-door.service) = success; test -s /var/lib/burrow/settings-door/wake.json")
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/code")
    machine.succeed("rm /var/lib/settings-fixture/limited")
    machine.wait_until_fails("burrow-settings /etc/settings-tracked allows sam/code", timeout=90)
    machine.succeed("test $(systemctl show -P Result burrow-settings-door.service) = success")
    assert "sam/code" in machine.succeed("settings show")
    assert "sam/code" in machine.succeed("runuser -u dev -- proj status")
    # The host write door has no pipeline dependency either.
    machine.succeed("runuser -u dev -- settings allow sam/code")
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/code")
    # Redelivery of the old request must leave the newer host decision alone.
    machine.succeed("systemctl start burrow-settings-door.service")
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/code")
    machine.succeed("runuser -u dev -- proj usage hold 4102444800 --only pipeline")
    machine.fail("burrow-settings /etc/settings-tracked allows sam/code")
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/box")
    assert "pipeline-only" in machine.succeed("runuser -u dev -- proj status")
    machine.succeed("runuser -u dev -- proj usage back")
    machine.succeed("burrow-settings /etc/settings-tracked allows sam/code")
    for unit in ["burrow-events", "proj-inbox", "proj-watch", "autoland", "autoland-review", "burrow-update", "burrow-ledger"]:
        machine.fail(f"systemctl is-active {unit}.service")
  '';
}
