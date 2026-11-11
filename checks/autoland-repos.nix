# A repo's settings in burrow.autoland.repos: each one the steward
# reads is taken, and one it doesn't (a typo, or one from a newer
# burrow) is refused rather than doing nothing. As update-origin
# does, it reads only autoland.nix's assertions.
{ self, pkgs, nixpkgs, ... }:
let
  lib = nixpkgs.lib;
  assertionsFor = repo: rc: let
    machine = self.nixosConfigurations.example.extendModules {
      modules = [{ burrow.autoland.repos.${repo} = rc; }];
    };
    mine = lib.filter (d: lib.hasSuffix "/modules/autoland.nix" (toString d.file)) machine.options.assertions.definitionsWithLocations;
  in lib.concatMap (d: d.value) mine;
  refused = repo: rc: lib.any (a: !a.assertion && lib.hasPrefix "burrow.autoland.repos: no such setting:" a.message
    && lib.hasInfix "\"${repo}\"." a.message) (assertionsFor repo rc);
  badTimeout = value: lib.any (a: !a.assertion &&
    a.message == "burrow.autoland.repos: proofTimeoutMinutes must be a positive integer.")
    (assertionsFor "sam/timeout" { proofTimeoutMinutes = value; });
  every = {
    rounds = 3; handoffAfter = 30; sessionHandoffAfter = 30; settleDelay = 5;
    stallAfter = 60; holdStallAfter = 60; renovateMergeAfter = 60; directPush = false; ruleset = true;
    requireCi = false; proofJobs.hil = "Run the bench."; proofBuilds.vm = "checks.x86_64-linux.vm";
    proofTimeoutMinutes = 720; proofRuns.gate = "make check"; proofSlots = 1; keepBranches = [ "keep/*" ]; codex = true;
    skipPaths = [ "^secret/" ]; train = false; trainSize = 8; trainRequire = [ ]; trainStallAfter = 60;
  };
  wrong = lib.filter (k: refused "sam/one" { ${k} = every.${k}; }) (lib.attrNames every)
    ++ map (k: "${k}, which is no setting") (lib.filter (k: !(refused "sam/typo" { ${k} = { }; }))
      # oneWay moved to Conary's class policy (burrow.ledger.policy).
      [ "proofRun" "keepBranch" "Rounds" "proofjobs" "slots" "oneWay" ])
    ++ lib.optional (lib.any (value: !(badTimeout value)) [ 0 (-1) "720" 1.5 null ]) "invalid proof timeout accepted"
    ++ lib.optional (badTimeout 720) "valid proof timeout refused";
in pkgs.runCommand "autoland-repos-check" { } ''
  ${lib.concatMapStrings (w: "echo 'wrong for ${w}'
") wrong}
  [ ${toString (builtins.length wrong)} = 0 ]
  touch $out
''
