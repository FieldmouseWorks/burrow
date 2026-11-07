# units: what's broken on the machine itself goes to the stall watchdog
# (crates/burrow-units). A unit that fails, and a program that dumps core,
# is an item in its watch file, due graceMinutes after it's seen; past that
# the watchdog hands it to a Claude job, which finds out why and fixes the
# cause, then to the stuck issue, then to the owner. A unit that runs again,
# or is reset, and a crash older than crashHours, leave the file.
#
# Every system service carries OnFailure=burrow-units-kick.service (a
# top-level service.d drop-in), and every core dump's systemd-coredump@
# OnSuccess= the same: that unit kicks burrow-units' burrow-events handler,
# which also passes on the hourly catch-up. No timer of its own. The kick
# and burrow-units itself mask that drop-in (one of the same name, linked to
# /dev/null, in their own .d: an empty OnFailure= can't take a dependency
# back): either failing would start the other, for ever. A burrow-units
# that stops passing goes stale, which the watchdog catches.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.watchdog.units;
  inherit (lib) mkOption types;
  inherit (config.burrow) user;
  name = "burrow-units";
  dir = "${config.burrow.stateDir}/units";
  file = "${dir}/watch.json";
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-units;
  inherit (config.burrow.events) kick;

  configFile = pkgs.writeText "${name}.json" (builtins.toJSON {
    inherit file;
    inherit (cfg) ignore;
    grace = cfg.graceMinutes * 60;
    window = cfg.crashHours * 3600;
    systemctl = "${config.systemd.package}/bin/systemctl";
    coredumpctl = "${config.systemd.package}/bin/coredumpctl";
    kick = if config.burrow.events.handlers ? watchdog then "${kick}/watchdog" else null;
  });
  # Anyone can look again: the watchdog's job, once it's fixed what it found.
  units = pkgs.writeShellScriptBin name ''
    export UNITS_CONFIG=''${UNITS_CONFIG:-${configFile}}
    exec ${package}/bin/${name} "$@"
  '';
  dropIns = pkgs.runCommand "${name}-drop-ins" { } ''
    d=$out/lib/systemd/system
    mkdir -p $d/service.d $d/systemd-coredump@.service.d $d/${name}.service.d $d/${name}-kick.service.d
    printf '[Unit]\nOnFailure=${name}-kick.service\n' >$d/service.d/90-${name}.conf
    printf '[Unit]\nOnSuccess=${name}-kick.service\n' >$d/systemd-coredump@.service.d/90-${name}.conf
    for unit in ${name} ${name}-kick; do
      ln -s /dev/null $d/$unit.service.d/90-${name}.conf
    done
  '';
in
{
  imports = [
    (import ../common.nix inputs)
    { key = "burrow/events"; imports = [ (import ../events.nix inputs) ]; }
    { key = "burrow/watchdog/sources"; imports = [ ./sources.nix ]; }
  ];

  options.burrow.watchdog.units = {
    enable = mkOption {
      type = types.bool;
      default = config.burrow.watchdog.enable or false;
      defaultText = lib.literalExpression "config.burrow.watchdog.enable";
      description = ''
        Failed units and programs that dumped core go to the stall watchdog,
        whose Claude job finds out why and fixes the cause.
      '';
    };

    ignore = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "flaky-sensor.service" "/nix/store/*/bin/game-server" ];
      description = ''
        Globs (`*`, `?`, `[...]`) of unit names and program paths it never
        watches: ones that fail by design.
      '';
    };

    graceMinutes = mkOption {
      type = types.ints.positive;
      default = 10;
      description = "How long a failure or crash waits before it's a stall: time for a restart, or a person, to get there first.";
    };

    crashHours = mkOption {
      type = types.ints.positive;
      default = 24;
      description = "How long a crash counts for: a program that crashed and hasn't since leaves the watch after this.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ units ];
    systemd.packages = [ dropIns ];

    burrow.events.handlers.units = {
      # Nothing on GitHub: its kicks, and the hourly catch-up.
      events = [ ];
      repos = [ ];
      perRepo = false;
      unit = name;
    };
    systemd.services.${name} = {
      description = "burrow-units: failed units and crashes, for the stall watchdog";
      # A pass a switch catches mid-run finishes; the next is the new one.
      restartIfChanged = false;
      serviceConfig = {
        Type = "oneshot";
        # Its watch file is the watchdog's to read, as the user's are.
        User = user;
        # Every user's core dumps are in the journal.
        SupplementaryGroups = [ "systemd-journal" ];
        Environment = "UNITS_CONFIG=${configFile}";
        ExecStart = "${package}/bin/${name}";
        TimeoutStartSec = "2min";
      };
    };
    systemd.services."${name}-kick" = {
      description = "Kick burrow-units: a unit failed, or a program dumped core";
      serviceConfig = {
        Type = "oneshot";
        DynamicUser = true;
        ExecStart = "${lib.getExe pkgs.curl} -fsS -m 10 -d \"\" ${kick}/units";
      };
    };
    systemd.tmpfiles.settings.burrow.${dir}.d = { mode = "0755"; inherit user; group = config.users.users.${user}.group; };

    burrow.watchdog.sources.units = {
      inherit file;
      about = "burrow-units, which watches for failed units and crashes";
      look = "`systemctl status UNIT` and `journalctl -b -u UNIT` say why a unit failed, `coredumpctl info EXE` why a program crashed. Fix the cause in the machine's flake or burrow; once it's live, the unit running again (or `systemctl reset-failed UNIT` for a one-off that's done) clears a unit, and `${name} clear EXE` (the path the item names) a program's crashes so far, each looking again as it does (`${name}` alone just looks). A crash after that is back in the watch.";
      kick = "units";
      # It writes on every pass, and has one at least hourly (burrow-events'
      # catch-up).
      staleMinutes = 120;
    };

    burrow.claude.machineMd = lib.mkOrder 1061 ''
      Failed units and programs that dumped core are the stall watchdog's too (`${name}`):
      ${toString cfg.graceMinutes} minutes after one is seen it goes to its Claude job. Units and programs that fail by
      design go in `burrow.watchdog.units.ignore`.
    '';
  };
}
