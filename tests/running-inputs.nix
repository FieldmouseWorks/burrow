# Input names need not resemble their repositories; revisions stay attached.
{ self, pkgs }:
let
  cfg = (self.inputs.nixpkgs.lib.nixosSystem {
    modules = [ self.nixosModules.switch {
      nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
      system.stateVersion = "26.05";
      burrow.switch = {
        enable = true;
        flakeInputs = {
          self.outPath = ./inputs-fixture;
          fetcher.rev = "1111111111111111111111111111111111111111";
          alias.rev = "2222222222222222222222222222222222222222";
          unknown.rev = "3333333333333333333333333333333333333333";
        };
      };
    } ];
  }).config;
  inputs = builtins.fromJSON cfg.environment.etc."burrow/inputs.json".text;
in
assert inputs.fetcher.repo == "sam/one";
assert inputs.alias.repo == "sam/two";
assert inputs.alias.rev == "2222222222222222222222222222222222222222";
assert inputs.unknown.repo == null;
true
