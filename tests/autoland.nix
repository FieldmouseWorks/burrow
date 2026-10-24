# autoland-steward's label handling, against a stand-in for GitHub: the
# labels it reads are made in every repo, and a PR at the round limit is
# escalated only once the owner label is on it.
#
#   nix build .#checks.x86_64-linux.autoland -L
{ pkgs }:
pkgs.runCommand "autoland-steward-test" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 -W error ${./autoland/test_steward.py} ${../modules/autoland/steward.py}
  touch $out
''
