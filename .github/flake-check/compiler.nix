# The workflow's compiler and linker, independent of a runner user's rustup
# configuration and of the caller's flake, registries and toolchain files.
let
  lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
  root = lock.nodes.${lock.root};
  locked = input: builtins.fetchTree lock.nodes.${root.inputs.${input}}.locked;
in
{ nixpkgs ? locked "nixpkgs"
, rust-overlay ? locked "rust-overlay"
, system ? builtins.currentSystem
}:
let
  pkgs = import nixpkgs { inherit system; config = { }; overlays = [ (import rust-overlay) ]; };
  spec = (builtins.fromTOML (builtins.readFile ../../crates/rust-toolchain.toml)).toolchain;
  # The crate's pinned channel, without its lint/format/development tools.
  compiler = pkgs.rust-bin.fromRustupToolchain (spec // { profile = "minimal"; components = [ ]; });
in pkgs.symlinkJoin {
  name = "flake-check-compiler";
  paths = [ compiler pkgs.stdenv.cc ];
}
