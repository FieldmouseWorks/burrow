# The flaky tool (docs/flaky-tests.md). It asks GitHub through the gh on its
# caller's PATH, the machine's (github.nix), which knows an App's env: one of
# its own would find no login there. The flake's checks run its tests
# (lib/project.nix) and that it has no gh of its own (flaky-gh).
{ lib, rustPlatform }:
rustPlatform.buildRustPackage {
  pname = "flaky";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  doCheck = false;
  meta = {
    description = "The GitHub side of quarantining flaky tests";
    license = lib.licenses.mit;
    mainProgram = "flaky";
  };
}
