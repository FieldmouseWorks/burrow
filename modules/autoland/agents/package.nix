# The shared CLI comes from the existing unstable flake input, so its
# source, runtime companions and dependency hashes advance together.
# Patch the owners of device grants and strict MCP configuration.
{ pkgs, nixpkgs-unstable }:
let
  unstable = nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system};
in unstable.codex.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../../patches/codex-job-capabilities.patch ];
  # The Rust setup hook requires system AWS-LC; OpenSSL cannot supply it.
  buildInputs = (old.buildInputs or [ ]) ++ [ unstable.aws-lc ];
  doCheck = true;
  cargoTestFlags = [
    "--package" "codex-config"
    "--package" "codex-linux-sandbox"
    "--lib" "job_capabilities_"
  ];
})
