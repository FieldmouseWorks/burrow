# Reject the configurations that can leave the revert behind an immortal stop,
# including a start that ignores SIGTERM. No exemption for restartIfChanged.
{ self, pkgs, nixpkgs, ... }:
let
  inherit (nixpkgs) lib;
  machine = manager: services: lib.nixosSystem {
    modules = [ self.nixosModules.switch {
      nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
      burrow.switch.enable = true;
      system.stateVersion = "26.05";
      systemd.settings.Manager = manager;
      systemd.services = services;
    } ];
  };
  refused = m: lib.concatMap (d: lib.filter (a: !a.assertion) d.value)
    (lib.filter (d: lib.hasSuffix "/modules/switch.nix" (toString d.file))
      m.options.assertions.definitionsWithLocations);
  service = sc: { serviceConfig = { ExecStart = "sleep infinity"; } // sc; };
  cases = {
    stop = service { TimeoutStopSec = "infinity"; ExecStop = "sleep infinity"; };
    post = service { TimeoutStopSec = "infinity"; ExecStopPost = "sleep infinity"; };
    zero.serviceConfig.TimeoutStopSec = 0;
    inherited.serviceConfig.TimeoutSec = "infinity";
    deaf = service { Type = "oneshot"; TimeoutStopSec = "infinity"; ExecStart = "sh -c 'trap \"\" TERM; exec sleep infinity'"; };
    unchanged = (service { TimeoutStopSec = "infinity"; }) // { restartIfChanged = false; stopIfChanged = false; };
    disabled = (service { TimeoutStopSec = "infinity"; }) // { enable = false; };
    overridden = service { TimeoutSec = "infinity"; TimeoutStopSec = "2s"; };
    finite = service { TimeoutStopSec = 150; };
    default = service { };
  };
  bad = refused (machine { } cases);
  good = refused (machine { } (removeAttrs cases [ "stop" "post" "zero" "inherited" "deaf" "unchanged" ]));
  infiniteDefault = refused (machine { DefaultTimeoutStopSec = "infinity"; } { });
in
assert lib.assertMsg (builtins.length bad == 1 && lib.hasInfix "Unbounded or unrecognised service timeouts: deaf, inherited, post, stop, unchanged, zero." (builtins.head bad).message) "switch accepted an unbounded stop";
assert lib.assertMsg (good == [ ]) "switch rejected bounded stops or a disabled unit";
assert lib.assertMsg (builtins.length infiniteDefault == 1) "switch accepted an infinite manager stop default";
pkgs.runCommand "switch-timeouts-check" { } "touch $out"
