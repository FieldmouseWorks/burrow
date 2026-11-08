# flake-gate (flake-gate.sh): a repo's CI gate, run here, with the machine's
# own nix.
{ writeShellApplication, git, yq-go, jq, rustc, stdenv }:
writeShellApplication {
  name = "flake-gate";
  runtimeInputs = [ git yq-go jq rustc stdenv.cc ];
  text = builtins.readFile ./flake-gate.sh;
}
