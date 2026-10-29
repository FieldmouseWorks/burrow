# flake-gate (flake-gate.sh): a repo's CI gate, run here, with the machine's
# own nix.
{ writeShellApplication, git, yq-go, jq }:
writeShellApplication {
  name = "flake-gate";
  runtimeInputs = [ git yq-go jq ];
  text = builtins.readFile ./flake-gate.sh;
}
