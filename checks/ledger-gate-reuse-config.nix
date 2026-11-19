# The ledger's gate-reuse lookup (burrow.ledger.gateReuse), as the module
# writes it: off, nothing of it, and the socket's conaryd as it is either
# way; on, its configuration takes whose attestations count from the train's
# own options (the signer workflow, the fresh runners' names), and nginx
# passes the vhost path to it. tests/ledger-gate-reuse.nix runs it.
{ self, pkgs, nixpkgs, cnStandIn, ... }:
let
  inherit (nixpkgs) lib;
  machine = modules: (lib.nixosSystem {
    modules = [
      self.nixosModules.ledger
      self.nixosModules.dispatch
      ../examples/machine.nix
      {
        burrow.ledger = { enable = true; daemon = true; package = cnStandIn; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; };
        burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; };
      }
    ] ++ modules;
  }).config;
  fresh = {
    burrow.dispatch = {
      enable = true;
      runners.ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; };
      fresh = { enable = true; owner = "sam-org"; repos = [ "sam-org/homelab" ]; };
    };
  };
  on = { burrow.ledger.gateReuse = { enable = true; repos = [ "sam/homelab" ]; path = "/conary/gate-reuse"; }; };
  signer = { burrow.ledger.signerWorkflow = "sam/ci/.github/workflows/gate.yml"; };

  off = machine [ ];
  hosted = machine [ on signer ];
  freshOn = machine [ on fresh ];
  freshOff = machine [ fresh ];

  unit = c: name: c.systemd.units.${name}.text;
  has = c: name: c.systemd.units ? ${name};
  checks = {
    "off, there's no lookup" = !(has off "conary-gate-reuse.service") && !(has off "conary-gate-reuse.path")
      && !(off.services.nginx.virtualHosts."example.org".locations ? "/conary/gate-reuse");
    "the socket's conaryd is the same with it on" = unit off "conaryd.service" == unit hosted "conaryd.service"
      && unit freshOff "conaryd.service" == unit freshOn "conaryd.service";
    "on, it's a unit of its own" = has hosted "conary-gate-reuse.service" && has hosted "conary-gate-reuse.path";
    "its signer is the train's" = hosted.burrow.ledger.gateReuse.settings.signer == "sam/ci/.github/workflows/gate.yml"
      && freshOn.burrow.ledger.gateReuse.settings.signer == freshOn.burrow.ledger.signerWorkflow;
    "its fresh runners are the train's" = freshOn.burrow.ledger.gateReuse.settings.fresh == freshOn.burrow.dispatch.fresh.prefix
      && freshOn.burrow.ledger.gateReuse.settings.fresh == freshOn.burrow.ledger.env.CONARY_FRESH_RUNNERS;
    "without fresh runners, GitHub's hosted runners alone" = hosted.burrow.ledger.gateReuse.settings.fresh == null;
    "nginx passes the vhost path to it" =
      hosted.services.nginx.virtualHosts."example.org".locations."/conary/gate-reuse".proxyPass == "http://127.0.0.1:8091"
      && hosted.burrow.ledger.gateReuse.url == "https://example.org/conary/gate-reuse";
  };
  failed = lib.attrNames (lib.filterAttrs (_: ok: !ok) checks);
in
if failed != [ ] then throw "ledger-gate-reuse-config: ${lib.concatStringsSep "; " failed}"
else pkgs.writeText "ledger-gate-reuse-config" (lib.concatStringsSep "\n" (lib.attrNames checks) + "\n")
