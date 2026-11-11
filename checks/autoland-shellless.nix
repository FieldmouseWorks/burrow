# The steward on a machine without /bin/sh: its sandbox's store still
# builds.
{ self, nixpkgs, cnStandIn, ... }:
let
  machine = nixpkgs.lib.nixosSystem {
    modules = [
      self.nixosModules.autoland
      ../examples/machine.nix
      ({ lib, pkgs, ... }: {
        burrow.autoland = { enable = true; owners = [ "sam" ]; authors = [ "sam" ]; app = { id = 123456; accountId = 123457; slug = "sams-steward"; keyFile = "/run/secrets/autoland-app-key"; }; reviewers.codex.enable = true; };
        # The steward needs the ledger; a stand-in for Conary's `cn`.
        burrow.ledger = { enable = true; package = cnStandIn; anchorRepo = "sam/homelab"; };
        burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; };
        environment.binsh = lib.mkForce null;
      })
    ];
  };
in builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward") machine.config.environment.systemPackages)
