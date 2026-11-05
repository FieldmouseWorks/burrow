# A repo's settings in burrow.autoland.repos: each one the steward
# reads is taken, and one it doesn't (a typo, or one from a newer
# burrow) is refused rather than doing nothing. As update-origin
# does, it reads only autoland.nix's assertions.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  refused = repo: rc: let
    machine = self.nixosConfigurations.example.extendModules {
      modules = [{ burrow.autoland.repos.${repo} = rc; }];
    };
    mine = lib.filter (d: lib.hasSuffix "/modules/autoland.nix" (toString d.file)) machine.options.assertions.definitionsWithLocations;
  in lib.any (a: !a.assertion && lib.hasPrefix "burrow.autoland.repos: no such setting:" a.message
    && lib.hasInfix "\"${repo}\"." a.message) (lib.concatMap (d: d.value) mine);
  every = {
    rounds = 3; oneWay = [ "^deploy/" ]; handoffAfter = 30; sessionHandoffAfter = 30; settleDelay = 5;
    stallAfter = 60; holdStallAfter = 60; renovateMergeAfter = 60; directPush = false; ruleset = true;
    requireCi = false; proofJobs.hil = "Run the bench."; proofBuilds.vm = "checks.x86_64-linux.vm";
    proofRuns.gate = "make check"; proofSlots = 1; keepBranches = [ "keep/*" ]; codex = true;
    skipPaths = [ "^secret/" ]; train = false; trainSize = 8; trainRequire = [ ]; trainStallAfter = 60;
  };
  wrong = lib.filter (k: refused "sam/one" { ${k} = every.${k}; }) (lib.attrNames every)
    ++ map (k: "${k}, which is no setting") (lib.filter (k: !(refused "sam/typo" { ${k} = { }; }))
      [ "proofRun" "keepBranch" "Rounds" "proofjobs" "slots" ]);
in pkgs.runCommand "autoland-repos-check" { } ''
  ${lib.concatMapStrings (w: "echo 'wrong for ${w}'
") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
