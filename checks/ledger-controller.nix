# The flow's controller user: only it can reach a producer (VM test).
{ self, pkgs, crates, system, ... }:
import ../tests/ledger-controller.nix {
  inherit self pkgs;
  # burrow-ledger-access's tests, built: those that set ACLs run in the VM.
  accessTests = crates.checks.${system}.rust-test.testBinaries "burrow-ledger-access";
} // {
  # The tests it runs, for the negative run (lib/project.nix's checks).
  testFiles = [ "tests/ledger-controller.nix" "crates/burrow-ledger-access/**" ];
}
