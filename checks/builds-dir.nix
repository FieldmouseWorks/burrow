# The build store's dir (burrow.builds.dir) on the example machine. Under a
# parent the machine's own config declares as another user's (scratchDir,
# a tmpfiles entry or a user's home), systemd-tmpfiles would refuse to make
# it, so evaluation fails and names that parent. Under root's, or the
# account's own, it doesn't.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  example = self.nixosConfigurations.example;
  failed = modules: map (a: a.message)
    (lib.filter (a: !a.assertion) (example.extendModules { inherit modules; }).config.assertions);
  at = dir: failed [ { burrow.builds.dir = lib.mkForce dir; } ];
  scratch = example.config.burrow.scratchDir;
  home = example.config.users.users.${example.config.burrow.user}.home;
  names = parent: ms: lib.any (lib.hasInfix "under ${parent} (declared owned by") ms;
  evals = {
    defaultClean = failed [ ] == [ ];
    varCacheClean = at "/var/cache/burrow-builds" == [ ];
    # tmpfiles.settings (updates.nix makes scratchDir the user's).
    scratchFails = names scratch (at "${scratch}/burrow-builds");
    # A home the machine makes.
    homeFails = names home (at "${home}/burrow-builds");
    # A plain tmpfiles rule, as another module would write it.
    ruleFails = names "/srv/caches" (failed [
      { burrow.builds.dir = lib.mkForce "/srv/caches/burrow-builds"; }
      { systemd.tmpfiles.rules = [ "d /srv/caches 0755 nobody nogroup -" ]; }
    ]);
    # Root's, or the account's own, is safe.
    rootRuleClean = failed [
      { burrow.builds.dir = lib.mkForce "/srv/caches/burrow-builds"; }
      { systemd.tmpfiles.rules = [ "d /srv/caches 0755 root root -" "d /srv/other 0755 nobody - -" ]; }
    ] == [ ];
    accountRuleClean = failed [
      { burrow.builds.dir = lib.mkForce "/srv/caches/store"; }
      { systemd.tmpfiles.rules = [ "d /srv/caches 0755 burrow-builds - -" ]; }
    ] == [ ];
  };
  wrong = lib.attrNames (lib.filterAttrs (_: ok: !ok) evals);
in pkgs.runCommand "builds-dir-check" { } ''
  ${lib.concatMapStrings (w: "echo 'builds.dir: ${w} is wrong'\n") wrong}
  ${lib.optionalString (wrong != [ ]) "exit 1"}
  touch $out
''
