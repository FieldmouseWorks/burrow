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
# replays, browsers, databases) stay its own and run in `nix develop`.
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

  toolchainFor = pkgs:
    let
      file = lib.findFirst builtins.pathExists null [
        (rustDir + "/rust-toolchain.toml")
        (rustDir + "/rust-toolchain")
        (self + "/rust-toolchain.toml")
        (self + "/rust-toolchain")
      ];
    in
    if file != null then pkgs.rust-bin.fromRustupToolchainFile file
    else pkgs.rust-bin.stable.latest.default.override { extensions = [ "rustfmt" "clippy" ]; };

  rustChecks = pkgs:
    let
      craneLib = (crane.mkLib pkgs).overrideToolchain (toolchainFor pkgs);
      # The whole directory, not only the Cargo sources: tests read fixtures.
      src = lib.cleanSource rustDir;
      common = { inherit src; strictDeps = true; };
      cargoArtifacts = craneLib.buildDepsOnly common;
    in
    {
      rust-fmt = craneLib.cargoFmt { inherit src; };
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
          ((toolchainFor pkgs).override { extensions = [ "rust-src" "rust-analyzer" ]; })
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
}
