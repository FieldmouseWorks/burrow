# The owner's plan, with one independent device login for each service user.
inputs:
{ config, lib, pkgs, ... }:
let
  proj = config.burrow.proj.enable or false;
  review = (config.burrow.autoland.enable or false) && (config.burrow.autoland.reviewers.codex.enable or false);
  inherit (import ./autoland/paths.nix config) reviewer reviewerHome;
  agents = import ./autoland/agents/package.nix { inherit pkgs; };
  package = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-codex;
  loginConfig = pkgs.writeText "codex-login.json" (builtins.toJSON {
    sudo = "/run/wrappers/bin/sudo";
    env = "${pkgs.coreutils}/bin/env";
    codex = "${agents}/bin/codex";
    targets = lib.optionalAttrs review { review = { user = reviewer; home = reviewerHome; }; }
      // lib.optionalAttrs proj { job = { user = config.burrow.user; home = config.users.users.${config.burrow.user}.home; }; };
  });
  login = pkgs.runCommand "autoland-codex-login" { nativeBuildInputs = [ pkgs.makeBinaryWrapper ]; } ''
    makeBinaryWrapper ${package}/bin/burrow-codex-login $out/bin/autoland-codex-login --add-flags ${loginConfig}
  '';
in
{
  config = lib.mkIf (proj || review) {
    environment.systemPackages = [ login ];
  };
}
