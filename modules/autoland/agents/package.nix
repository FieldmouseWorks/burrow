# The shared CLI comes from the existing unstable flake input, so its
# source, runtime companions and dependency hashes advance together.
# Patch the owner of explicit device mounts rather than wrap the CLI.
{ pkgs, nixpkgs-unstable }:
(nixpkgs-unstable.legacyPackages.${pkgs.stdenv.hostPlatform.system}.codex).overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../../patches/codex-device-grants.patch ];
})
