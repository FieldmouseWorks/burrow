# A deferred engines pass must survive a scheduler restart and run without
# another issue event. Keep engine receipts private while exposing only the
# wake booking to the scheduler's separate account.
{ self, pkgs }:
let
  gh = pkgs.writeShellScriptBin "gh" ''
    echo asked >>/var/lib/door-fixture/calls
    if [ -e /var/lib/door-fixture/limited ]; then
      printf 'HTTP/2.0 403\r\nContent-Type: application/json\r\nRetry-After: 20\r\n\r\n{"message":"API rate limit exceeded"}'
      exit 1
    fi
    printf 'HTTP/2.0 200\r\nContent-Type: application/json\r\n\r\n[]'
  '';
in
pkgs.testers.runNixOSTest {
  name = "engines-door";
  nodes.machine = { config, ... }: {
    imports = [ self.nixosModules.proj ];
    users.users.dev.isNormalUser = true;
    burrow.claude.package = pkgs.hello;
    burrow.authors = [ { login = "sam"; id = 1; } ];
    burrow.proj = {
      enable = true;
      inherit gh;
      engines.door = {
        enable = true;
        repo = "sam/box";
      };
    };
    # Prove readable wake permissions even with a restrictive service umask.
    systemd.services.burrow-engines-door.serviceConfig.UMask = "0077";
    systemd.tmpfiles.rules = [
      "d /var/lib/door-fixture 0755 dev users -"
      "f /var/lib/door-fixture/limited 0644 dev users -"
    ];
    environment.etc."test-engines-wake".text = config.burrow.events.handlers.engines.wake;
  };
  testScript = ''
    import json

    machine.wait_for_unit("burrow-events.service")
    wake = machine.succeed("cat /etc/test-engines-wake").strip()
    machine.wait_until_succeeds(f"test -s {wake}")
    machine.wait_until_succeeds("test $(systemctl show -P ActiveState burrow-engines-door.service) = inactive")
    machine.succeed("test $(systemctl show -P Result burrow-engines-door.service) = success")

    # Control: the old layout really is unreadable by the scheduler account.
    private = "/var/lib/burrow/proj-engines"
    machine.succeed(f"cp {wake} {private}/old-wake.json")
    machine.fail(f"runuser -u burrow-events -- cat {private}/old-wake.json")
    assert machine.succeed(f"stat -c %a {private}").strip() == "700"

    booked = json.loads(machine.succeed(f"runuser -u burrow-events -- cat {wake}"))
    assert booked.get("", 0) > 0, booked
    machine.succeed("rm /var/lib/door-fixture/limited")
    machine.succeed("systemctl restart burrow-events.service")
    machine.wait_for_unit("burrow-events.service")
    machine.wait_until_succeeds(f"test $(cat {wake}) = '{{}}'", timeout=45)
    assert len(machine.succeed("cat /var/lib/door-fixture/calls").split()) == 2
    machine.succeed("test $(systemctl show -P Result burrow-engines-door.service) = success")
    assert machine.succeed(f"stat -c %a {private}").strip() == "700"
  '';
}
