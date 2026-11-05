# flake-gate: a repo's CI gate step, run here as CI runs it, and
# burrow's own evaluating apart, so a Claude job's gate holds little;
# and the gate's own scripts, its negative run's too, shellchecked
# with the build.sh they source.
{ pkgs, ... }:
pkgs.runCommand "flake-gate-check" { nativeBuildInputs = [ (pkgs.callPackage ../tools/flake-gate/package.nix { }) pkgs.git pkgs.yq-go pkgs.shellcheck ]; } ''
  shellcheck -s bash ${../tests/flake-gate.sh}
  shellcheck -s bash -x -P SCRIPTDIR ${../.github/flake-check}/gate.sh ${../.github/flake-check}/negative.sh
  bash ${../tests/flake-gate.sh} ${../.}
  touch $out
''
