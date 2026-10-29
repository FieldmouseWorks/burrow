# The flaky tool (docs/flaky-tests.md), with gh, which it asks GitHub through,
# on its PATH. The flake's checks run its tests (lib/project.nix).
{ lib, rustPlatform, makeWrapper, gh }:
rustPlatform.buildRustPackage {
  pname = "flaky";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./src ];
  };
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ makeWrapper ];
  doCheck = false;
  postInstall = ''
    wrapProgram $out/bin/flaky --prefix PATH : ${lib.makeBinPath [ gh ]}
  '';
  meta = {
    description = "The GitHub side of quarantining flaky tests";
    license = lib.licenses.mit;
    mainProgram = "flaky";
  };
}
