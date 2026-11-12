# Source blocks, their local copies, the Claude import and guidance text.
# Guide growth (with its PR-body reason) stays in the house action.
{ self, pkgs, system, ... }:
pkgs.runCommand "guidance-check" { } ''
  ${self.packages.${system}.burrow-house}/bin/burrow-house guidance ${self}
  touch $out
''
