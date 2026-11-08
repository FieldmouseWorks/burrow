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
in
assert builtins.elem "burrow-boot-check.service" trial.after;
assert builtins.elem "burrow-boot-check.service" watch.after;
assert builtins.elem "multi-user.target" watch.wantedBy;
assert watch.serviceConfig.Restart == "always";
assert path.pathConfig.Unit == "burrow-boot-trial.service";
assert builtins.elem "/run/burrow-switch" path.pathConfig.PathChanged;
assert cfg.systemd.timers.burrow-boot-trial.timerConfig.OnCalendar == "*-*-* 05:15:00";
pkgs.runCommand "boot-config" { } "touch $out"
