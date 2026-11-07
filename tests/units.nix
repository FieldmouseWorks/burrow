# burrow-units, for real: every service carries OnFailure= its kick, and
# every core dump OnSuccess= it, but the kick and burrow-units itself don't
# (either failing would start the other for ever). A unit that fails, and a
# program that dumps core, land in its watch file and kick the watchdog (a
# stand-in here, noting each kick); a unit that runs again leaves it once
# burrow-units looks again, and a crash once it's cleared, until the next.
# What an item says, and when it's due, are crates/burrow-units' tests.
#
#   nix build .#checks.x86_64-linux.units -L
{ self, pkgs }:
let
  watchdog = pkgs.writeShellScript "watchdog-stand-in" ''
    date +%s >>/var/lib/watchdog-kicks/kicks
  '';
  broken = pkgs.writeShellScript "broken" ''
    [ -e /run/broken-fixed ] || { echo "not yet"; exit 3; }
  '';
in
pkgs.testers.runNixOSTest {
  name = "units";

  nodes.machine = {
    imports = [ self.nixosModules.watchdog ];
    users.users.dev = { isNormalUser = true; group = "dev"; };
    users.groups.dev = { };
    burrow.events = {
      enable = true;
      host = "example.org";
      secretFile = "/etc/burrow-webhook";
      handlers.watchdog = {
        events = [ ];
        repos = [ ];
        perRepo = false;
        unit = "watchdog-stand-in";
      };
    };
    environment.etc."burrow-webhook".text = "hook-secret\n";
    burrow.watchdog.units.enable = true;
    systemd.services.watchdog-stand-in.serviceConfig = {
      Type = "oneshot";
      ExecStart = "${watchdog}";
      StateDirectory = "watchdog-kicks";
    };
    systemd.services.broken.serviceConfig = {
      Type = "oneshot";
      ExecStart = "${broken}";
    };
  };

  testScript = ''
    import json

    def watch():
        return json.loads(machine.succeed("cat /var/lib/burrow/units/watch.json"))["items"]

    def kicks():
        return len(machine.succeed("cat /var/lib/watchdog-kicks/kicks 2>/dev/null || true").split())

    machine.wait_for_unit("burrow-events.service")
    machine.wait_for_open_port(8788)

    with subtest("every service kicks it when it fails, but not the kick or itself"):
        for unit in ("broken", "sshd", "burrow-events"):
            machine.succeed(f"systemctl show -p OnFailure --value {unit}.service | grep -x burrow-units-kick.service")
        for unit in ("burrow-units", "burrow-units-kick"):
            assert machine.succeed(f"systemctl show -p OnFailure --value {unit}.service").strip() == "", unit

    with subtest("a failed unit is in its watch file, and the watchdog is kicked"):
        machine.wait_until_succeeds("test -e /var/lib/burrow/units/watch.json", timeout=120)
        before = kicks()
        machine.fail("systemctl start broken.service")
        machine.wait_until_succeeds("grep -q unit:broken.service /var/lib/burrow/units/watch.json", timeout=120)
        item = watch()["unit:broken.service"]
        assert item["phase"] == "failed (exit-code)", item
        assert item["due"] == item["since"] + 600, item
        machine.wait_until_succeeds(f"[ $(wc -l </var/lib/watchdog-kicks/kicks) -gt {before} ]", timeout=60)

    with subtest("a program that dumps core is in it too"):
        machine.succeed("su - dev -c 'ulimit -c unlimited; sh -c \"kill -SEGV \\$\\$\"' || true")
        machine.wait_until_succeeds("coredumpctl list --no-pager | grep -q bash", timeout=60)
        machine.wait_until_succeeds("grep -q '\"core:' /var/lib/burrow/units/watch.json", timeout=120)
        cores = [k for k in watch() if k.startswith("core:")]
        assert len(cores) == 1 and cores[0].endswith("/bin/bash"), watch()

    with subtest("a crash cleared once fixed leaves it, and a later one is back"):
        exe = cores[0][len("core:"):]
        machine.succeed(f"su - dev -c 'burrow-units clear {exe}'")
        assert cores[0] not in watch(), watch()
        machine.succeed("sleep 1; su - dev -c 'ulimit -c unlimited; sh -c \"kill -SEGV \\$\\$\"' || true")
        machine.wait_until_succeeds(f"grep -qF '\"{cores[0]}\"' /var/lib/burrow/units/watch.json", timeout=120)
        assert "crashed (last:" in watch()[cores[0]]["what"], watch()

    with subtest("a unit that runs again leaves it once it looks again"):
        machine.succeed("touch /run/broken-fixed && systemctl start broken.service")
        machine.succeed("su - dev -c burrow-units")
        assert "unit:broken.service" not in watch(), watch()
  '';
}
