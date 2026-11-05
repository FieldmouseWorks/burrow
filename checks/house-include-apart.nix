# And a file outside them changes none of its Rust checks, with the
# repo a store path, as a flake's self is: a path into it would make
# them depend on all of it, and run again for any change at all.
{ self, pkgs, nixpkgs, system, ... }:
  let
    fixture = "tests/house/rust-include";
    # The repo as a store path, as a flake's self is, with only the
    # fixture and the toolchain file its own links to, less what
    # `keep` leaves out.
    copy = keep: (self.lib.project {
      self = builtins.path {
        path = ../.;
        name = "source";
        filter = path: _:
          let rel = nixpkgs.lib.removePrefix "${toString ../.}/" (toString path); in
          keep rel && nixpkgs.lib.any (p: rel == p || nixpkgs.lib.hasPrefix "${p}/" rel || nixpkgs.lib.hasPrefix "${rel}/" p)
            [ fixture "crates/rust-toolchain.toml" ];
      };
      systems = [ system ];
      rust = { dir = "${fixture}/ws"; include = [ "${fixture}/spec/fact.txt" ]; };
    }).checks.${system};
    all = copy (_: true);
    less = copy (rel: rel != "${fixture}/spec/other.txt");
    moved = builtins.filter (n: nixpkgs.lib.hasPrefix "rust-" n && all.${n}.drvPath != less.${n}.drvPath) (builtins.attrNames all);
  in
  pkgs.runCommand "house-include-apart-check" { } (
    if moved == [ ] then "touch $out"
    else "echo ${nixpkgs.lib.escapeShellArg "${fixture}/spec/other.txt, which nothing includes, changes ${toString moved}"}; exit 1"
  )
