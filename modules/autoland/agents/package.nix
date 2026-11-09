# The shared CLI comes from the existing unstable flake input, so its
# source, runtime companions and dependency hashes advance together.
# Patch the owners of device grants and strict MCP configuration.
{ pkgs, nixpkgs-unstable }:
(nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system}.codex).overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../../patches/codex-job-capabilities.patch ];
  doCheck = true;
  cargoTestFlags = [
    "--package" "codex-config"
    "--package" "codex-linux-sandbox"
    "--lib" "job_capabilities_"
  ];
})
