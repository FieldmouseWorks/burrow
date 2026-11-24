# The machine's Conary is the one burrow's crates link (lib/conary.nix): a
# ledger package built from another rev fails the machine's build, as remi's
# would have on 2026-11-22 (its Conary input a commit past the crates' pin),
# when the judge refused every request whose judging context the two computed
# apart. One built from the pin passes, the default is built from it, and the
# packages lib.project builds say their rev, so a flake's Conary is held too.
{ self, pkgs, nixpkgs, system, cnStandIn, ... }:
let
  inherit (nixpkgs) lib;
  pin = self.lib.conary.rev;
  # A rev that isn't the pin, as a Conary input bumped past it would be.
  other = builtins.hashString "sha1" "not ${pin}";
  machine = package: (lib.nixosSystem {
    modules = [
      self.nixosModules.ledger
      ../examples/machine.nix
      { burrow.ledger = { enable = true; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; } // package; }
    ];
  }).config;
  failing = c: map (a: a.message) (builtins.filter (a: !a.assertion) c.assertions);
  pinMessages = c: builtins.filter (lib.hasInfix "burrow's crates link Conary") (failing c);
  # A project's package, built from a commit.
  project = (self.lib.project {
    self = { outPath = ../tests/house/rust; rev = other; };
    systems = [ system ];
    rust = { };
  }).packages.${system};
  checks = {
    "a Conary at another rev fails the build" = pinMessages (machine { package = cnStandIn // { rev = other; }; }) != [ ];
    "a Conary at the pin passes" = pinMessages (machine { package = cnStandIn // { rev = pin; }; }) == [ ];
    "the default is Conary at the pin" = let c = machine { }; in pinMessages c == [ ] && c.burrow.ledger.package.rev == pin;
    "burrow's Conary says its rev" = self.packages.${system}.conary.rev == pin;
    "a project's package says its commit" = project.default.rev == other;
    # rust.members: burrow's own, each a package of its own.
    "a project's members say their commit" = self.packages.${system}.burrow-ledger-access.rev == (self.rev or self.dirtyRev);
  };
  failed = lib.attrNames (lib.filterAttrs (_: ok: !ok) checks);
in
if failed != [ ] then throw "ledger-conary-pin: ${lib.concatStringsSep "; " failed}"
else pkgs.writeText "ledger-conary-pin" (lib.concatStringsSep "\n" (lib.attrNames checks) + "\n")
