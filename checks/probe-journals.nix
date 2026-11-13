# Every unit burrow's own modules define is on the probe's journal
# allowlist, system and user alike, so the job a failed unit is handed to
# can read why it failed. A new module's unit, or a template's instance,
# can't fall off it: this reads the units from the modules themselves.
# Units burrow only tunes (sshd, say) are upstream's; the ones below whose
# logs can hold a secret stay off by design, and so do unrelated units.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  machine = self.nixosConfigurations.example;
  # As burrow-events' glob reads them: `*` any run, `?` any one, a
  # `.` itself (no unit name or allowlist entry holds another
  # character a regex reads otherwise).
  regex = lib.replaceStrings [ "." "*" "?" ] [ "\\." ".*" "." ];
  on = globs: unit: lib.any (g: builtins.match (regex g) unit != null) globs;
  # Whether a definition holds, through the mkIf, mkMerge and priority
  # wrappers a module puts round it: a unit only a false mkIf names isn't
  # one the machine has.
  live = v:
    if v._type or null == "if" then v.condition && live v.content
    else if v._type or null == "merge" then lib.any live v.contents
    else if lib.elem (v._type or null) [ "override" "order" ] then live v.content
    else true;
  # The units a definition makes.
  names = v:
    if v._type or null == "if" then lib.optionals v.condition (names v.content)
    else if v._type or null == "merge" then lib.concatMap names v.contents
    else if lib.elem (v._type or null) [ "override" "order" ] then names v.content
    else lib.attrNames (lib.filterAttrs (_: live) v);
  ours = d: lib.hasPrefix "${toString self}/modules/" (toString d.file);
  # A unit only burrow's modules define, as a template's instance.
  defined = option:
    let
      defs = option.definitionsWithLocations;
      upstream = lib.concatMap (d: names d.value) (lib.filter (d: !ours d) defs);
      units = lib.subtractLists upstream (lib.unique (lib.concatMap (d: names d.value) (lib.filter ours defs)));
    in map (u: if lib.hasSuffix "@" u then "${u}sam-homelab.service" else "${u}.service") units;
  # Kept off: gh-token's units mint and hand out the Apps' tokens.
  secret = [ "gh-token-*" ];
  unrelated = [ "sshd.service" "renovate-secrets.service" "unrelated.service" "gh-token-workers-access.service" ];
  scopes = {
    system = { globs = machine.config.burrow.probe.journal.units; units = defined machine.options.systemd.services; };
    user = { globs = machine.config.burrow.probe.journal.userUnits; units = defined machine.options.systemd.user.services; };
  };
  wrong = lib.concatLists (lib.mapAttrsToList (scope: s:
    map (u: "${scope} ${u}: not on the probe allowlist")
      (lib.filter (u: !on s.globs u && !on secret u) s.units)) scopes);
  leaked = lib.filter (on scopes.system.globs) unrelated;
in
# The units the steward, Renovate and house-sync hand to jobs on failure,
# so a change to how units are read can't empty the list unnoticed.
assert lib.assertMsg (lib.all (u: lib.elem u scopes.system.units) [
  "autoland.service" "autoland@sam-homelab.service" "renovate.service" "renovate@sam-homelab.service"
  "house-sync.service" "house-sync@sam-homelab.service"
]) "probe-journals: burrow's units: ${builtins.toJSON scopes.system.units}";
assert lib.assertMsg (leaked == [ ]) "probe-journals: on the allowlist but must stay off: ${builtins.toJSON leaked}";
pkgs.runCommand "probe-journals-check" { } ''
  ${lib.concatMapStrings (w: "echo ${lib.escapeShellArg w}\n") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
