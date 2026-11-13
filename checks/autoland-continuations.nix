# Continuation receipts and new-work events must reach different commands
# and wake files, with the same authentication and hard timeout.
{ self, pkgs, ... }:
let
  machine = self.nixosConfigurations.example.config;
  valid = m:
    let
      handlers = m.burrow.events.handlers;
      services = m.systemd.services;
    in handlers ? autoland-continue
      && services ? autoland-continue
      && services ? "autoland-continue@"
      && handlers.autoland-continue.events == [ ]
      && handlers.autoland.wake != handlers.autoland-continue.wake
      && pkgs.lib.hasSuffix " continue" services.autoland-continue.serviceConfig.ExecStart
      && pkgs.lib.hasSuffix " continue %I" services."autoland-continue@".serviceConfig.ExecStart
      && services.autoland-continue.serviceConfig.LoadCredential == services.autoland.serviceConfig.LoadCredential
      && services.autoland-continue.serviceConfig.TimeoutStartSec == services.autoland.serviceConfig.TimeoutStartSec
      && services."autoland-continue@".unitConfig.OnSuccess == services."autoland@".unitConfig.OnSuccess;
  withoutHandler = machine // {
    burrow = machine.burrow // {
      events = machine.burrow.events // {
        handlers = builtins.removeAttrs machine.burrow.events.handlers [ "autoland-continue" ];
      };
    };
  };
  sharedWake = machine // {
    burrow = machine.burrow // {
      events = machine.burrow.events // {
        handlers = machine.burrow.events.handlers // {
          autoland-continue = machine.burrow.events.handlers.autoland-continue // {
            wake = machine.burrow.events.handlers.autoland.wake;
          };
        };
      };
    };
  };
in
assert valid machine;
assert !(valid withoutHandler);
assert !(valid sharedWake);
pkgs.runCommand "autoland-continuations-check" { } "touch $out"
