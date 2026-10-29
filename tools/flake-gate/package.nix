# flake-gate (flake-gate.sh): a repo's CI gate, run here. nix, and gh for a
# workflow another repo pins, are the machine's own (the toolbox has gh).
{ writeShellApplication, git, yq-go, jq }:
writeShellApplication {
  name = "flake-gate";
  runtimeInputs = [ git yq-go jq ];
  text = builtins.readFile ./flake-gate.sh;
}
