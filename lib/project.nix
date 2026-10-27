# burrow.lib.project: a project's dev shell and its gate, so each repo's
# flake.nix is a few lines and gets better when burrow does (Renovate bumps
# the burrow input like any other).
#
#   outputs = { self, burrow }: burrow.lib.project {
#     inherit self;
#     rust = { };                      # or { dir = "client"; }
#     python = { };
#   };
#
# `nix develop` gets the toolchains; `nix flake check` is the gate: what it
# runs is the same on a laptop, in an agent's sandbox and in CI. The checks
# are hermetic (the Nix sandbox, no network). A repo's heavier gates (VM
# replays, browsers, databases) stay its own and run in `nix develop`. A Rust
# workspace's binaries are its packages.default, for machines to install.
{ nixpkgs, rust-overlay, crane }:
{
  self,
  systems ? [ "x86_64-linux" "aarch64-linux" ],
  # Rust: fmt, clippy (all targets, warnings are errors) and the tests of the
  # Cargo workspace in DIR, built with the toolchain DIR/rust-toolchain.toml
  # (or the repo root's) pins; latest stable without one. `test = false` skips
  # the tests (say, when they need a database).
  rust ? null,
  # Python: ruff's error rules (syntax errors, undefined names) over DIRS.
  # `format = true` also holds them to `ruff format`.
  python ? null,
  # Node: the current LTS in the dev shell. Its checks stay in the repo's own
  # CI, which needs node_modules.
  node ? null,
  # More dev shell tools: pkgs: [ pkgs.godot ].
  packages ? (_: [ ]),
  # More hermetic checks: pkgs: { name = derivation; }.
  checks ? (_: { }),
}:
let
  lib = nixpkgs.lib;
  forAll = f: lib.genAttrs systems (system: f (import nixpkgs {
    inherit system;
    overlays = [ rust-overlay.overlays.default ];
  }));

  rustCfg = { dir = "."; test = true; } // (if rust == null then { } else rust);
  pyCfg = { dirs = [ "." ]; format = false; } // (if python == null then { } else python);
  rustDir = if rustCfg.dir == "." then self else self + "/${rustCfg.dir}";

  # What the repo's rust-toolchain(.toml) asks for, as rust-overlay's
  # fromRustupToolchain takes it; latest stable with rustfmt and clippy
  # without one.
  toolchainSpec =
    let
      file = lib.findFirst builtins.pathExists null [
        (rustDir + "/rust-toolchain.toml")
        (rustDir + "/rust-toolchain")
        (self + "/rust-toolchain.toml")
        (self + "/rust-toolchain")
      ];
      text = builtins.readFile file;
      # The legacy rust-toolchain file is either TOML or just the channel.
      spec =
        if lib.hasInfix "[toolchain]" text then (builtins.fromTOML text).toolchain
        else { channel = lib.trim text; };
    in
    { components = [ ]; } // (if file != null then spec else { channel = "stable"; components = [ "rustfmt" "clippy" ]; });

  # extra: components on top of the repo's own (the shell's rust-analyzer).
  toolchainFor = pkgs: extra:
    pkgs.rust-bin.fromRustupToolchain (toolchainSpec // {
      components = toolchainSpec.components ++ extra;
    });

  craneFor = pkgs: (crane.mkLib pkgs).overrideToolchain (toolchainFor pkgs [ ]);

  # The workspace's binaries, built for release: packages.default, what a
  # machine installs.
  rustPackage = pkgs: (craneFor pkgs).buildPackage {
    src = (craneFor pkgs).cleanCargoSource rustDir;
    strictDeps = true;
    cargoExtraArgs = "--locked --workspace";
    # The gate runs the tests.
    doCheck = false;
  };

  rustChecks = pkgs:
    let
      craneLib = craneFor pkgs;
      # The whole directory, not only the Cargo sources: tests read fixtures.
      src = lib.cleanSource rustDir;
      # Every member of the workspace, not only a root package or its
      # default-members, built as `cargo test` builds them (crane's default is
      # release, which drops debug assertions and overflow checks).
      common = { inherit src; strictDeps = true; cargoExtraArgs = "--locked --workspace"; CARGO_PROFILE = "dev"; };
      cargoArtifacts = craneLib.buildDepsOnly common;
    in
    {
      rust-fmt = craneLib.cargoFmt { inherit src; cargoExtraArgs = "--all"; };
      rust-clippy = craneLib.cargoClippy (common // {
        inherit cargoArtifacts;
        cargoClippyExtraArgs = "--all-targets -- -D warnings";
      });
    } // lib.optionalAttrs rustCfg.test {
      rust-test = craneLib.cargoTest (common // { inherit cargoArtifacts; });
    };

  pyChecks = pkgs:
    let dirs = lib.escapeShellArgs pyCfg.dirs; in
    {
      python-ruff = pkgs.runCommand "python-ruff" { nativeBuildInputs = [ pkgs.ruff ]; } ''
        cd ${self}
        ruff check --no-cache --select E9,F63,F7,F82 ${dirs}
        ${lib.optionalString pyCfg.format "ruff format --no-cache --check ${dirs}"}
        touch $out
      '';
    };
in
{
  devShells = forAll (pkgs: {
    default = pkgs.mkShell {
      packages =
        lib.optional (rust != null)
          (toolchainFor pkgs [ "rust-src" "rust-analyzer" ])
        ++ lib.optionals (python != null) [ pkgs.python3 pkgs.ruff pkgs.uv ]
        ++ lib.optional (node != null) pkgs.nodejs
        ++ packages pkgs;
    };
  });

  checks = forAll (pkgs:
    lib.optionalAttrs (rust != null) (rustChecks pkgs)
    // lib.optionalAttrs (python != null) (pyChecks pkgs)
    // checks pkgs);

  formatter = forAll (pkgs: pkgs.nixfmt-rfc-style);
} // lib.optionalAttrs (rust != null) {
  packages = forAll (pkgs: { default = rustPackage pkgs; });
}
