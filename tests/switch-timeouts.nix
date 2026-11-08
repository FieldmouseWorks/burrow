# Reject the configurations that can leave the revert behind an immortal stop,
# including a start that ignores SIGTERM. No exemption for restartIfChanged.
{ self, pkgs, nixpkgs }:
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
  # Assert the exact rejected names so another bad service cannot hide a
  # bypass. Lists render as repeated settings, not Nix's toString.
  abortService = sc: service ({ TimeoutStopSec = "2s"; TimeoutStopFailureMode = "abort"; } // sc);
  abortBad = [
    (abortService { TimeoutAbortSec = "infinity"; })
    (abortService { TimeoutAbortSec = 0; ExecStop = "sleep infinity"; })
    (abortService { TimeoutAbortSec = "infinity"; ExecStopPost = "sleep infinity"; })
    ((abortService { Type = "oneshot"; TimeoutAbortSec = "infinity"; }) // { restartIfChanged = false; stopIfChanged = false; })
    (service { TimeoutStartFailureMode = "abort"; TimeoutAbortSec = "infinity"; })
    (service { TimeoutStopFailureMode = [ "abort" ]; TimeoutAbortSec = "infinity"; })
  ];
  # Independent boundary fixtures from systemd's parse_time: it rejects n >=
  # UINT64_MAX / multiplier (in microseconds), including equality.
  boundaries = [
    { firstInvalid = 18446744073709; aliases = [ "" "s" "sec" "second" "seconds" ]; }
    { firstInvalid = 307445734561; aliases = [ "m" "min" "minute" "minutes" ]; }
    { firstInvalid = 5124095576; aliases = [ "h" "hr" "hour" "hours" ]; }
    { firstInvalid = 213503982; aliases = [ "d" "day" "days" ]; }
  ];
  boundaryValues = delta: [ (18446744073709 + delta) ] ++ lib.concatMap (b:
    map (alias: "${toString (b.firstInvalid + delta)}${alias}") b.aliases) boundaries;
  invalid = boundaryValues 0 ++ [ true false [ true ] [ "2s" "infinity" ]
    "99999999999999s" "99999999999999999999999999999999s"
    "18446744073710s" "213503983d" "9223372036854775807d" ];
  named = values: f: lib.listToAttrs (lib.imap0 (i: v:
    lib.nameValuePair "probe-${toString i}-case" (f v)) values);
  invalidServices = (named invalid (v: service { TimeoutSec = "infinity"; TimeoutStopSec = v; }))
    // lib.mapAttrs' (n: v: lib.nameValuePair "inherit-${n}" v) (named invalid (v: service { TimeoutSec = v; }))
    // lib.mapAttrs' (n: v: lib.nameValuePair "abort-${n}" v) (named invalid (v: abortService { TimeoutAbortSec = v; }));
  badServices = (lib.filterAttrs (n: _: builtins.elem n [ "stop" "post" "zero" "inherited" "deaf" "unchanged" ]) cases)
    // invalidServices
    // lib.mapAttrs' (n: v: lib.nameValuePair "mode-${n}" v) (named abortBad (s: s));
  goodServices = (removeAttrs cases [ "stop" "post" "zero" "inherited" "deaf" "unchanged" ])
    // lib.mapAttrs' (n: v: lib.nameValuePair "finite-${n}" v)
      (named [ { } { TimeoutAbortSec = ""; } { TimeoutAbortSec = "3s"; } ] abortService)
    // lib.mapAttrs' (n: v: lib.nameValuePair "boundary-${n}" v) (named (boundaryValues (-1))
      (v: abortService { TimeoutSec = "infinity"; TimeoutStopSec = v; TimeoutAbortSec = v; }));
  mixed = refused (machine { } (badServices // goodServices));
  inherited = refused (machine { DefaultTimeoutAbortSec = "infinity"; } {
    bad = abortService { };
    override = abortService { TimeoutAbortSec = "2s"; };
    reset = abortService { TimeoutAbortSec = ""; };
    disabled = (abortService { TimeoutAbortSec = "infinity"; }) // { enable = false; };
  });
  badDefaults = v: builtins.length (refused (machine
    { DefaultTimeoutStopSec = v; DefaultTimeoutAbortSec = v; }
    { probe = abortService { }; })) == 1;
  abortBoundaryDefault = v: builtins.length (refused (machine
    { DefaultTimeoutAbortSec = v; } { probe = abortService { }; })) == 1;
in
assert lib.assertMsg (builtins.length mixed == 1 && lib.hasInfix
  ("Unbounded or unrecognised service timeouts: " + lib.concatStringsSep ", " (lib.attrNames badServices) + ".")
  (builtins.head mixed).message) "switch accepted an unbounded or invalid stop/abort timeout";
assert lib.assertMsg (builtins.length inherited == 1 && lib.hasInfix
  "Unbounded or unrecognised service timeouts: bad." (builtins.head inherited).message)
  "switch mishandled abort timeout inheritance, override or reset";
assert lib.assertMsg (lib.all badDefaults
  ([ "infinity" true [ "2s" true ] "99999999999999999999999999999999d" 18446744073709 ]
    ++ map (b: "${toString b.firstInvalid}${lib.last b.aliases}") boundaries))
  "switch accepted an invalid manager timeout";
assert lib.assertMsg (lib.all abortBoundaryDefault
  ([ 18446744073709 ] ++ map (b: "${toString b.firstInvalid}${lib.last b.aliases}") boundaries))
  "switch accepted an invalid manager abort boundary";
assert lib.assertMsg (refused (machine
  { DefaultTimeoutStopSec = 18446744073708; DefaultTimeoutAbortSec = "18446744073708seconds"; }
  { probe = service { TimeoutStopFailureMode = "abort"; }; }) == [ ])
  "switch rejected valid manager boundaries";
true
