# Completion events must drive trials; the calendar only backs them up.
{ self, pkgs, nixpkgs, ... }:
let
  machine = nixpkgs.lib.nixosSystem {
    modules = [
      self.nixosModules.switch
      ../examples/machine.nix
      {
        burrow.switch = { enable = true; trialBoot.enable = true; };
      }
    ];
  };
  cfg = machine.config;
  trial = cfg.systemd.services.burrow-boot-trial;
  watch = cfg.systemd.services.burrow-boot-trial-watch;
  path = cfg.systemd.paths.burrow-boot-trial;
  withRunner = machine.extendModules {
    modules = [
      self.nixosModules.dispatch
      { burrow.dispatch.runners.ci = { repo = "sam/homelab"; labels = [ "self-hosted" ]; }; }
    ];
  };
  changedWatch = withRunner.config.systemd.services.burrow-boot-trial-watch;
  inherit (nixpkgs) lib;
  names = [
    "burrow-boot-check.service"
    "burrow-boot-trial.service"
    "burrow-boot-trial-watch.service"
    "burrow-boot-trial.path"
    "burrow-boot-trial.timer"
  ];
  # Keep the generated unit graph, substituting only the boot executable:
  # verification must never build or execute a real boot/reboot command.
  unitText = name:
    let
      text = builtins.unsafeDiscardStringContext cfg.systemd.units.${name}.text;
      command = if lib.hasSuffix ".service" name
        then builtins.unsafeDiscardStringContext cfg.systemd.services.${lib.removeSuffix ".service" name}.serviceConfig.ExecStart
        else null;
    in if command == null then text
      else lib.replaceStrings [ command ] [ "${pkgs.coreutils}/bin/true" ] text;
  files = pkgs.linkFarm "boot-graph-units" (map (name: {
    inherit name;
    path = pkgs.writeText name (unitText name);
  }) names);
  wanted = target: lib.filter (name:
    builtins.elem target cfg.systemd.units.${name}.wantedBy) names;
in
assert builtins.elem "burrow-boot-check.service" trial.after;
assert builtins.elem "burrow-boot-check.service" watch.after;
assert builtins.elem "multi-user.target" watch.wantedBy;
assert watch.serviceConfig.Restart == "always";
# Switches must restart the listener when its captured work patterns change.
assert watch.restartIfChanged && changedWatch.restartIfChanged;
assert watch.serviceConfig.ExecStart != changedWatch.serviceConfig.ExecStart;
assert path.pathConfig.Unit == "burrow-boot-trial.service";
assert builtins.elem "/run/burrow-switch" path.pathConfig.PathChanged;
assert cfg.systemd.timers.burrow-boot-trial.timerConfig.OnCalendar == "*-*-* 05:15:00";
pkgs.runCommand "boot-config" { nativeBuildInputs = [ pkgs.systemd ]; } ''
  mkdir units
  cp ${files}/* units/
  # Targets provide the real target default-dependency ordering. Their Wants
  # are the generated installation links, not a separate list of boot units.
  cat >units/multi-user.target <<'UNIT'
  [Unit]
  Wants=${lib.concatStringsSep " " (wanted "multi-user.target")} timers.target
  UNIT
  cat >units/timers.target <<'UNIT'
  [Unit]
  Wants=${lib.concatStringsSep " " (wanted "timers.target")}
  UNIT
  for target in sysinit basic shutdown paths; do
    printf '[Unit]\nDefaultDependencies=no\n' >"units/$target.target"
  done
  mkdir runtime
  # Target Wants/After default ordering is identical in user and system
  # managers. User-scope verification keeps the offline manager's runtime
  # files inside the build sandbox instead of trying to create /run/systemd.
  export XDG_RUNTIME_DIR="$PWD/runtime"
  export SYSTEMD_UNIT_PATH="$PWD/units" SYSTEMD_LOG_TARGET=console
  systemd-analyze --user verify --generators=no --man=no multi-user.target burrow-boot-trial.service >verify.log 2>&1 || {
    cat verify.log
    exit 1
  }
  cat verify.log
  # verify may return success after dropping a wanted job to break a cycle.
  if grep -q 'ordering cycle' verify.log; then
    exit 1
  fi
  touch $out
''
