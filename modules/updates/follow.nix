# follow: a Cargo pin on another repo's default branch kept at its head
# (crates/cargo-follow). A manifest says in its `metadata.follow` which git
# URL it follows and what changes there are inert; a push to either repo's
# default branch starts a pass over that repo's follows, which opens (or
# refreshes) one PR moving the pin when something past it counts, and opens
# none when nothing does. The steward lands it like any other.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.updates.follow;
  inherit (config.burrow) user;
  inherit (lib) mkOption types;
  inherit (config.burrow.github) machineApp machineGh;
  autoland = lib.attrByPath [ "burrow" "autoland" ] { } config;
  stateDir = "${config.burrow.stateDir}/cargo-follow";

  followConfig = pkgs.writeText "cargo-follow.json" (builtins.toJSON {
    inherit (cfg) manifests;
    work = stateDir;
    # A PR the train carries keeps its head: a new one would throw away the
    # candidate it rides in, gate and all.
    frozen = lib.optional (autoland.enable or false) autoland.labels.waiting.train.name;
  });
  # gh and git as the machine's own App, when it has one; cargo writes the
  # lock file.
  follow = pkgs.runCommand "cargo-follow" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "cargo-follow";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.cargo-follow}/bin/cargo-follow \
      $out/bin/cargo-follow --add-flags ${followConfig} \
      --prefix PATH : ${lib.makeBinPath [ machineGh config.programs.git.package pkgs.cargo ]}
  '';
in
{
  imports = [
    { key = "burrow/events"; imports = [ (import ../events.nix inputs) ]; }
    { key = "burrow/github"; imports = [ (import ../github.nix inputs) ]; }
  ];

  options.burrow.updates.follow = {
    enable = lib.mkEnableOption "cargo-follow, each listed manifest's Cargo pins kept at the head of the repo they follow";

    manifests = mkOption {
      type = types.listOf (types.submodule {
        options = {
          repo = mkOption {
            type = types.str;
            example = "sam/homelab";
            description = "The repo the manifest is in, OWNER/NAME.";
          };
          path = mkOption {
            type = types.str;
            default = "Cargo.toml";
            example = "crates/Cargo.toml";
            description = "The manifest's path in it.";
          };
        };
      });
      default = [ ];
      description = ''
        The manifests whose `[workspace.metadata.follow."URL"]` (or
        `[package.metadata.follow."URL"]`) tables it keeps: each table's URL
        is the `git` of the dependencies it moves, and its `inert` and
        `inert-lines` what changes there move nothing (crates/cargo-follow).
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ follow ];
    # Every repo's push to its default branch: the follower's own or an
    # upstream's, which only the manifests name. A pass about neither reads
    # the manifests and stops. burrow-events' catch-up is the full pass.
    burrow.events.handlers.cargo-follow = {
      events = [ "push" ];
      unit = "cargo-follow";
      pushes = "default";
    };
    systemd.tmpfiles.rules = [ "d ${stateDir} 0755 ${user} ${config.users.users.${user}.group} -" ];
    systemd.services = lib.genAttrs [ "cargo-follow" "cargo-follow@" ] (name: {
      description = "cargo-follow: ${if name == "cargo-follow" then "every manifest's" else "%I's"} Cargo pins at the head they follow";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      environment = lib.optionalAttrs (machineApp != null) machineApp.env;
      serviceConfig = {
        Type = "oneshot";
        User = user;
        ExecStart = "${follow}/bin/cargo-follow${lib.optionalString (name == "cargo-follow@") " %I"}";
        TimeoutStartSec = "20min";
        Nice = 10;
      };
    });
  };
}
