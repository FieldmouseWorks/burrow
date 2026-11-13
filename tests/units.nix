# burrow-units, for real: its watcher follows the journal and asks for a
# pass each time a unit fails or a program dumps core, touching no unit's
# own OnFailure= (a service whose failure isolates a target still loads).
# A unit that fails, and a program that dumps core, whoever ran it, land in
# its watch file and kick the watchdog (a stand-in here, noting each kick);
# a unit that runs again leaves it once burrow-units is asked to look
# again, and a crash once it's cleared, until the next. Only the service
# passes (a pass that can't read the whole journal is refused), so a clear
# by a user who can't keeps everyone else's crashes. What an item says, and when it's due, are
# crates/burrow-units' tests.
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
    # Its failure isolates a target, which allows it no other OnFailure=.
    systemd.services.isolating = {
      onFailure = [ "rescue.target" ];
      unitConfig.OnFailureJobMode = "isolate";
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.coreutils}/bin/true";
      };
    };
  };

  testScript = ''
    import json

    watch_file = "/var/lib/burrow/units/watch.json"

    def watch():
        return json.loads(machine.succeed(f"cat {watch_file}"))["items"]

    def kicks():
        return len(machine.succeed("cat /var/lib/watchdog-kicks/kicks 2>/dev/null || true").split())

    def dev_crash():
        machine.succeed("su - dev -c 'ulimit -c unlimited; sh -c \"kill -SEGV \\$\\$\"' || true")

    # sleep, once it's exec'd: a program of its own, run by root.
    def root_crash():
        machine.succeed("bash -c 'ulimit -c unlimited; sleep 60 & p=$!; sleep 1; kill -SEGV $p; wait $p' || true")

    machine.wait_for_unit("burrow-events.service")
    machine.wait_for_open_port(8788)
    machine.wait_for_unit("burrow-units-watch.service")

    with subtest("no unit's own failure settings are touched"):
        assert machine.succeed("systemctl show -p LoadState --value isolating.service").strip() == "loaded"
        assert machine.succeed("systemctl show -p OnFailure --value isolating.service").strip() == "rescue.target"
        assert machine.succeed("systemctl show -p OnFailure --value broken.service").strip() == ""

    with subtest("a failed unit is in its watch file, and the watchdog is kicked"):
        # The watcher asks for a pass as it starts.
        machine.wait_until_succeeds(f"test -e {watch_file}", timeout=120)
        before = kicks()
        machine.fail("systemctl start broken.service")
        machine.wait_until_succeeds(f"grep -q unit:broken.service {watch_file}", timeout=120)
        item = watch()["unit:broken.service"]
        assert item["phase"] == "failed (exit-code)", item
        assert item["due"] == item["since"] + 600, item
        machine.wait_until_succeeds(f"[ $(wc -l </var/lib/watchdog-kicks/kicks) -gt {before} ]", timeout=60)

    with subtest("a program that dumps core is in it too, whoever ran it"):
        dev_crash()
        root_crash()
        machine.wait_until_succeeds(f"[ $(grep -o '\"core:[^\"]*\"' {watch_file} | sort -u | wc -l) -eq 2 ]", timeout=120)
        cores = [k for k in watch() if k.startswith("core:")]
        bash = next(k for k in cores if k.endswith("/bin/bash"))
        # sleep's own program: coreutils, where it's one binary for all.
        root = next(k for k in cores if k != bash)

    with subtest("an isolated caller clears host crashes, everyone else's stay, and a later one is back"):
        # dev can't read root's crashes itself, so they stay only if the
        # pass that follows its clear is the service's.
        assert root[len("core:"):] not in machine.succeed("su - dev -c 'coredumpctl list --no-pager --json=short' 2>&1 || true")
        # And a pass of its own, which would drop them, it's refused.
        before = machine.succeed(f"cat {watch_file}")
        machine.fail("su - dev -c 'burrow-units pass'")
        assert machine.succeed(f"cat {watch_file}") == before
        exe = bash[len("core:"):]
        # The same empty-root boundary as a sandboxed job: system programs
        # and only the clear socket, with no host state or journals. The
        # host configuration is loaded by the socket-activated service.
        isolated = "${pkgs.bubblewrap}/bin/bwrap --ro-bind /nix/store /nix/store --ro-bind /run/current-system/sw /run/current-system/sw --ro-bind /run/burrow-units/clear.sock /run/burrow-units/clear.sock --proc /proc --dev /dev --tmpfs /tmp --"
        machine.succeed(f"su - dev -c '{isolated} /run/current-system/sw/bin/test ! -e /var/lib/burrow/units'")
        before = json.loads(machine.succeed(f"cat {watch_file}"))["at"]
        machine.succeed(f"su - dev -c '{isolated} /run/current-system/sw/bin/burrow-units clear {exe}'")
        acknowledged = json.loads(machine.succeed("cat /var/lib/burrow/units/cleared.json"))
        assert exe in acknowledged, acknowledged
        assert acknowledged[exe] >= before, acknowledged
        machine.wait_until_succeeds(f"! grep -qF '\"{bash}\"' {watch_file}", timeout=60)
        assert json.loads(machine.succeed(f"cat {watch_file}"))["at"] > before
        assert root in watch(), watch()
        machine.succeed("sleep 1")
        dev_crash()
        machine.wait_until_succeeds(f"grep -qF '\"{bash}\"' {watch_file}", timeout=120)
        assert "crashed (last:" in watch()[bash]["what"], watch()

    with subtest("a unit that runs again leaves it once it's asked to look again"):
        machine.succeed("touch /run/broken-fixed && systemctl start broken.service")
        machine.succeed("su - dev -c burrow-units")
        machine.wait_until_succeeds(f"! grep -q unit:broken.service {watch_file}", timeout=60)
  '';
}
