# HOST-update's limit on real systemd: burrow-update itself, in a unit with
# the example machine's own settings at a smaller scale, with stand-ins for
# what it runs. A wait for a slot doesn't use the limit up through a
# daemon-reload or a re-exec, while it waits or from its own switch
# (systemd keeps no extended timeout across a reload), and work past
# the limit still ends it, with all it started.
#
#   nix build .#checks.x86_64-linux.update-limit -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  example = self.nixosConfigurations.example.config;
  tool = example.burrow.tools.update;
  real = example.systemd.services.${tool.meta.mainProgram or tool.name};
  program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-update}/bin/burrow-update";
  limit = 20;
  stand = name: text: pkgs.writeShellScript name text;
  nix = pkgs.writeShellScriptBin "nix" ''
    case "$*" in
      "flake metadata"*) echo '{"locks":{"nodes":{"root":{"inputs":{}}}}}' ;;
      *'#checks.'*) ;;
      *) echo /t/new ;;
    esac
  '';
  config = pkgs.writeText "update.json" (builtins.toJSON {
    update = "box-update";
    host = "box";
    state = "/t/state";
    work = "/t/work";
    user = "root";
    group = "root";
    uid = 0;
    home = "/t/home";
    runuser = stand "runuser" ''shift 3; exec "$@"'';
    path = lib.makeBinPath [ nix pkgs.git pkgs.coreutils ];
    configuration = "box";
    system = "x86_64-linux";
    heavy = stand "heavy" ''
      shift 2
      touch /t/waiting
      while [ ! -e /t/release ]; do sleep 0.2; done
      exec "$@"
    '';
    inherit program;
    project = "homelab";
    app = null;
    train = null;
    origin = "/t/origin.git";
    flake = "/t/operator";
    owner = "Sam";
    manifest = null;
    releases = "https://downloads.example/claude";
    checks = [ "vm" ];
    within = "15min";
    switchState = "/t/switch";
    currentSystem = "/t/current";
    lock = "/t/lock";
    timers = [ ];
    tools = lib.genAttrs [ "sudo" "notify" "vulnix" "curl" "install" "logger" "systemctl" ] (name: stand name "exit 0") // {
      boot = null;
      # As switch-to-configuration does: PID 1 reloads, then there's more.
      switch = stand "switch" ''
        [ "$1" = --toplevel ] || exit 0
        touch /t/switching
        ${pkgs.systemd}/bin/systemctl daemon-reload
        sleep "$(cat /t/activation)"
      '';
      confirm = stand "confirm" "touch /t/confirmed";
      health = stand "health" ''[ "$1" != snapshot ] || touch "$2"; echo healthy'';
    };
  });
  # A main with one commit that the machine doesn't run yet, and a clone of it.
  fixture = pkgs.writeShellScript "fixture" ''
    set -e
    rm -rf /t
    mkdir -p /t/home /t/state/logs /t/state/jobs /t/switch /t/old /t/new
    g="${pkgs.git}/bin/git -c user.name=t -c user.email=t@example.org"
    $g init -q -b main /t/seed
    echo v1 >/t/seed/flake.lock
    $g -C /t/seed add flake.lock
    $g -C /t/seed commit -qm v1
    $g clone -q --bare /t/seed /t/origin.git
    $g clone -q /t/origin.git /t/work
    ln -s /t/old /t/current
    echo /t/old >/t/state/deployed
    echo "$1" >/t/activation
  '';
in
pkgs.testers.runNixOSTest {
  name = "update-limit";

  nodes.machine = {
    systemd.services.box-update = {
      restartIfChanged = false;
      path = [ pkgs.coreutils ];
      # Whichever of the unit's own settings bound it, at the test's scale.
      environment = lib.optionalAttrs (real.environment ? BURROW_UPDATE_WORK_LIMIT) {
        BURROW_UPDATE_WORK_LIMIT = toString limit;
      };
      serviceConfig = real.serviceConfig // {
        ExecStart = "${program} ${config}";
        TimeoutStartSec =
          if real.serviceConfig.TimeoutStartSec == "infinity" then "infinity" else limit;
      };
    };
  };

  testScript = ''
    import time

    failed = []

    def shows(what):
        return machine.succeed(f"systemctl show box-update -P {what}").strip()

    def started(activation):
        machine.execute("systemctl stop box-update; systemctl reset-failed box-update")
        machine.succeed(f"${fixture} {activation}")
        machine.succeed("systemctl start --no-block box-update")
        machine.wait_for_file("/t/waiting", timeout=30)

    def ended():
        machine.wait_until_succeeds(
            "case $(systemctl show box-update -P ActiveState) in inactive|failed) ;; *) false ;; esac",
            timeout=120)
        return shows("Result")

    def check(case, held, said):
        print(f"update-limit: {case}: {'held' if held else 'FAILED'} ({said})")
        if not held:
            failed.append(f"{case}: {said}")

    def lived(case):
        result = ended()
        confirmed = machine.execute("test -e /t/confirmed")[0] == 0
        check(case, result == "success" and confirmed, f"{result}, confirmed: {confirmed}")

    machine.wait_for_unit("multi-user.target")

    with subtest("a reload while it waits, past the limit"):
        started(activation=2)
        time.sleep(${toString limit} + 5)
        machine.succeed("systemctl daemon-reload")
        time.sleep(2)
        machine.succeed("touch /t/release")
        lived("a reload while it waits, past the limit")

    with subtest("a re-exec while it waits, past the limit"):
        started(activation=2)
        time.sleep(${toString limit} + 5)
        machine.succeed("systemctl daemon-reexec")
        time.sleep(2)
        machine.succeed("touch /t/release")
        lived("a re-exec while it waits, past the limit")

    with subtest("its switch's reload, past the limit"):
        started(activation=2)
        time.sleep(${toString limit} + 5)
        machine.succeed("touch /t/release")
        lived("its switch's reload, past the limit")

    with subtest("its switch's reload, before the limit"):
        started(activation=14)
        time.sleep(12)
        machine.succeed("touch /t/release")
        lived("its switch's reload, before the limit")

    with subtest("work past the limit"):
        started(activation=300)
        time.sleep(12)
        machine.succeed("touch /t/release")
        machine.wait_for_file("/t/switching", timeout=15)
        result = ended()
        confirmed = machine.execute("test -e /t/confirmed")[0] == 0
        left = machine.execute("pgrep -xf 'sleep 300'")[0] == 0
        check("work past the limit", result != "success" and not confirmed and not left,
              f"{result}, confirmed: {confirmed}, left running: {left}")

    assert not failed, "; ".join(failed)
  '';
}
