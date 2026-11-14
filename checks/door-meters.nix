# Each door's BURROW_DOOR_METER is the meter of the App its own gh acts
# as (burrow.github.appOf), never machinery's by default: an override as
# another App is metered as that App, a user's login or a gh no App owns
# by none, and a machine with no machinery has no door meter at all.
{ self, pkgs, ... }:
let
  inherit (pkgs) lib;
  doors = extra: (self.nixosConfigurations.example.extendModules {
    modules = [ {
      burrow.proj.engines.door = { enable = true; repo = "sam/homelab"; label = "switch-engines"; };
      burrow.settings.door = { enable = true; repo = "kim/settings"; authors = [ { login = "sam"; id = 1; } ]; };
    } extra ];
  }).config;
  meters = config: lib.mapAttrs (_: unit: let m = config.systemd.services.${unit}.environment.BURROW_DOOR_METER;
    in if m == "" then null else builtins.fromJSON m) {
    probe = "burrow-probe";
    ledger = "burrow-ledger-door";
    engines = "burrow-engines-door";
    settings = "burrow-settings-door";
  };
  as = app: owner: { dir = self.nixosConfigurations.example.config.burrow.github.quotaDir; inherit app owner; };
  tools = {
    burrow.github.apps.tools = { id = 9; accountId = 10; slug = "sam-tools"; keyFile = "/run/secrets/tools-key"; };
  };
  # The default: every door's gh is machinery's.
  plain = meters (doors { });
  # Overrides: another App's gh, the user's own login, a stand-in that says
  # which App it is, and one that doesn't.
  overridden = meters (doors ({ config, ... }: {
    imports = [ tools ];
    burrow.probe.gh = config.burrow.github.apps.tools.gh;
    burrow.ledger.gh = pkgs.gh;
    burrow.proj.gh = config.burrow.github.apps.tools.gh;
    burrow.settings.door.gh = pkgs.writeShellScriptBin "gh" "exit 1";
  }));
  declared = meters (doors ({ config, ... }: {
    imports = [ tools ];
    burrow.proj.gh = pkgs.writeShellScriptBin "gh" "exit 1";
    burrow.proj.hostGithubApp = "tools";
  }));
  bare = meters (doors { burrow.github.machinery = lib.mkForce null; });
  expect = name: got: want:
    if got == want then true
    else throw "${name}: got ${builtins.toJSON got}, want ${builtins.toJSON want}";
in
assert expect "plain" plain {
  probe = as "workers" "sam";
  ledger = as "workers" "sam";
  engines = as "workers" "sam";
  settings = as "workers" "kim";
};
assert expect "overridden" overridden {
  probe = as "tools" "sam";
  ledger = null;
  engines = as "tools" "sam";
  settings = null;
};
assert expect "declared" declared.engines (as "tools" "sam");
assert expect "bare" bare { probe = null; ledger = null; engines = null; settings = null; };
pkgs.runCommand "door-meters-check" { } "touch $out"
