# flake-gate: a repo's CI gate step, run here as CI runs it, and
# burrow's own evaluating apart, so a Claude job's gate holds little;
# and the shell bootstraps for the Rust gate and negative runner.
{ pkgs, crates, system, ... }:
pkgs.runCommand "flake-gate-check" {
  FLAKE_CHECK_RUNNER = crates.packages.${system}.burrow-flake-check;
  passthru.testFiles = [ "tests/flake-gate.sh" ];
  nativeBuildInputs = [ (pkgs.callPackage ../tools/flake-gate/package.nix { }) pkgs.git pkgs.yq-go pkgs.shellcheck ];
} ''
  shellcheck -s bash ${../tests/flake-gate.sh}
  shellcheck -s bash -x -P SCRIPTDIR ${../.github/flake-check}/gate.sh ${../.github/flake-check}/negative.sh ${../.github/flake-check}/run.sh
  bash ${../tests/flake-gate.sh} ${../.}
  touch $out
''
