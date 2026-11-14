# HOST-escalation: the one way the owner label goes on an issue or PR
# (crates/burrow-escalation). A producer names a reason; only a one-way door
# this machine lists (ownerReasons) reaches the owner, and everything else is
# the operator's, under the owner's standing authority. An owner escalation
# goes to the owner's project first, through the coordinator's relay, and the
# label goes on only once the coordinator has acknowledged it there, as one
# durable operation that a failure or restart carries on rather than repeat.
inputs:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.burrow.escalation;
  inherit (config.burrow) user;
  name = "${config.burrow.prefix}-escalation";

  dir = "${config.burrow.stateDir}/escalations";

  # Every reason there is, in crates/burrow-escalation's order (reason.rs):
  # the settings schema holds this list and its default to that one.
  reasons = [
    "secret-create"
    "secret-rotate"
    "secret-delete"
    "zfs-destroy-or-rollback"
    "disk-wipe-or-partition"
    "remote-data-delete"
    "default-branch-force-push"
    "firmware-flash"
    "hosting-cancel-or-reinstall"
    "sign-in"
    "two-factor"
    "confirm-access"
  ];

  # The owner label proj and the steward put on: the steward's, else the
  # sorter's.
  ownerLabel = lib.attrByPath [ "burrow" "autoland" "labels" "owner" ] (lib.attrByPath [
    "burrow"
    "proj"
    "inbox"
    "triage"
    "ownerLabel"
  ] "needs-owner" config) config;

  escalationConfig = pkgs.writeText "${name}.json" (
    builtins.toJSON {
      inherit
        name
        dir
        user
        ownerLabel
        ;
      owner = config.burrow.owner;
      ownerReasons = cfg.ownerReasons;
      relay =
        if cfg.relay.repo == null then
          null
        else
          {
            inherit (cfg.relay) repo label;
            acknowledgers = map (a: { inherit (a) login id; }) cfg.relay.acknowledgers;
          };
      home = config.users.users.${user}.home;
    }
  );
  escalation =
    pkgs.runCommand name
      {
        nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
        meta.mainProgram = name;
      }
      ''
        makeBinaryWrapper ${
          inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-escalation
        }/bin/burrow-escalation \
          $out/bin/${name} --add-flags ${escalationConfig} \
          --prefix PATH : ${
            lib.makeBinPath [
              pkgs.coreutils
              pkgs.util-linux
              config.burrow.github.machineGh
            ]
          }
      '';
in
{
  imports = [
    (import ./common.nix inputs)
    {
      key = "burrow/github";
      imports = [ (import ./github.nix inputs) ];
    }
  ];

  options.burrow.escalation = {
    enable = lib.mkEnableOption "HOST-escalation, the owner label only for one-way doors, delivered to the owner's project first";

    ownerReasons = lib.mkOption {
      type = lib.types.listOf (lib.types.enum reasons);
      default = reasons;
      description = ''
        The one-way doors that may wait on the owner: creating, rotating or
        deleting a secret; the irreversible actions (destroying or rolling
        back a ZFS dataset, wiping or partitioning a disk, deleting remote
        data, force-pushing a default branch, flashing firmware, the hosting
        provider's cancel or reinstall); and a sign-in, two-factor or
        Confirm access prompt only a person can clear. Every other request,
        and one of these taken off the list, goes to the operator.
      '';
    };

    relay = {
      repo = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "sam/homelab";
        description = ''
          The repo (OWNER/NAME) the owner's project coordinator reads for
          requests: each owner escalation opens one issue there, as the
          machine's App. null: no route to the owner's project, so nothing
          goes to the owner and the operator raises it.
        '';
      };

      label = lib.mkOption {
        type = lib.types.str;
        default = "escalation";
        description = "The label on each request issue. The inbox never sorts or hands one on.";
      };

      acknowledgers = lib.mkOption {
        type = lib.types.listOf (
          lib.types.submodule {
            options = {
              login = lib.mkOption { type = lib.types.str; };
              id = lib.mkOption {
                type = lib.types.ints.positive;
                description = "The account's numeric id: what's matched.";
              };
            };
          }
        );
        default = [ ];
        example = [
          {
            login = "sam";
            id = 1;
          }
        ];
        description = ''
          The accounts whose `delivered KEY` comment on a request says the
          owner's project has it (the coordinator's GitHub login, matched by
          id). Only then does the owner label go on.
        '';
      };
    };
  };

  config = lib.mkMerge [
    # Always built, so the modules that call it work whether or not it's on PATH.
    { burrow.tools.escalation = escalation; }

    (lib.mkIf cfg.enable {
      assertions = [
        {
          assertion = cfg.relay.repo == null || cfg.relay.acknowledgers != [ ];
          message = "burrow.escalation.relay.repo needs acknowledgers: with none, no request is ever delivered.";
        }
      ];
      environment.systemPackages = [ escalation ];
      systemd.tmpfiles.settings.burrow = {
        ${config.burrow.stateDir}.d = {
          mode = "0755";
          user = "root";
          group = "root";
        };
        ${dir}.d = {
          mode = "0750";
          inherit user;
          group = config.users.users.${user}.group;
        };
      };
    })
  ];
}
