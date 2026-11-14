# The dispatcher's config as the module writes it, read by the binary
# (the example's, and one with a deploy to watch): which commit of
# each watched input the running system has, from the flake's
# flake.lock (tests/dispatch), with its own repo left to the
# configuration revision the switch stamps, so that a build from a
# dirty tree and the same tree committed are one system; and each
# runner's labels,
# GitHub's defaults for the machine's architecture among them unless
# it has none; and the `.runner` of each that's checked against GitHub's
# list of runners, whose path unit alone it may stop besides the runners.
{ self, pkgs, nixpkgs, cnStandIn, ... }:
let
  inherit (nixpkgs) lib;
  machine = own: modules: lib.nixosSystem {
    modules = [
      self.nixosModules.dispatch
      self.nixosModules.updates
      self.nixosModules.ledger
      ../examples/machine.nix
      {
        burrow.ledger = { package = cnStandIn; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; };
        burrow.dispatch = {
          enable = true;
          runners = {
            ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; registration = "/var/lib/ci/runner/.runner"; };
            bare = { repo = "sam/homelab"; labels = [ "bare" ]; defaultLabels = false; };
          };
          fresh = { enable = true; owner = "sam-org"; repos = [ "sam-org/homelab" ]; };
        };
        burrow.updates = {
          enable = true;
          origin = "https://github.com/sam/homelab";
          watch = [ "sam/homelab" "sam/kit" "Sam/Tools" "sam/elsewhere" ];
        };
        burrow.switch.flakeInputs = {
          self = { outPath = ../tests/dispatch; } // own;
          kit.rev = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
          tools.rev = "cccccccccccccccccccccccccccccccccccccccc";
          nixpkgs.rev = "dddddddddddddddddddddddddddddddddddddddd";
        };
      }
    ] ++ modules;
  };
  committed = machine { rev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; } [ ];
  privateRepos = machine { } [ {
    imports = [ self.nixosModules.autoland ];
    burrow.autoland = {
      enable = true;
      owners = [ "sam" ];
      app = { id = 123; accountId = 124; slug = "sams-steward"; keyFile = "/run/secrets/steward"; };
    };
  } ];
  dispatchOnly = machine { } [ { burrow.dispatch.fresh.enable = lib.mkForce false; } ];
  # More kept waiting than there may be at once.
  overKept = machine { } [ { burrow.dispatch.fresh.idle = 3; } ];
  refused = m: builtins.filter (a: !a.assertion) m.config.assertions;
  dirty = machine { dirtyRev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dirty"; } [ ];
  arm = machine { } [ { nixpkgs.hostPlatform = lib.mkForce "aarch64-linux"; } ];
  # remi's install rehearsal turns burrow-events' handlers off.
  unhandled = machine { rev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; } [ { burrow.events.handlers = lib.mkForce { }; } ];
  # A machine may space the passes out itself.
  spaced = machine { } [ { burrow.events.handlers.dispatch.gap = lib.mkForce 600; } ];
  settings = m: m.config.burrow.dispatch.settings;
  revisions = map (m: m.config.system.configurationRevision) [ committed dirty ];
  deploy = { inherit ((settings committed).deploy) own running; };
  want = {
    own = "sam/homelab";
    running = {
      "sam/kit" = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
      "Sam/Tools" = "cccccccccccccccccccccccccccccccccccccccc";
    };
  };
  labels = m: lib.mapAttrs (_: r: r.labels) (settings m).runners;
  wantLabels = arch: { ci-runner = [ "self-hosted" "Linux" arch "homelab" ]; bare = [ "bare" ]; };
  # The fresh runners: what a pass reads, the repo's queued jobs
  # among its events, and a unit that starts as a user and in a
  # state directory nothing had before, with its registration as a
  # credential only it reads, and that nothing outlives.
  wantFresh = {
    owner = "sam-org"; group = 1; repos = [ "sam-org/homelab" ]; label = "fresh-burrow";
    prefix = "fresh-burrow-"; most = 2; idle = 0; template = "burrow-fresh-runner@"; configs = "/run/burrow-fresh";
  };
  freshUnit = committed.config.systemd.services."burrow-fresh-runner@";
  # Each setting in the section of the unit file systemd reads it
  # from: one in the wrong one is ignored.
  freshWant = {
    Unit = [ "CollectMode=inactive-or-failed" ];
    Service = [
      "Type=exec" "DynamicUser=true" "StateDirectory=burrow-fresh-runner/%i"
      "LoadCredential=jit:/run/burrow-fresh/%i" "Restart=no" "KillMode=control-group"
    ];
  };
  freshSections = lib.listToAttrs (map (part:
    let lines = lib.splitString "\n" part;
    in lib.nameValuePair (lib.removeSuffix "]" (builtins.head lines)) (builtins.tail lines)
  ) (builtins.tail (lib.splitString "\n[" ("\n" + builtins.unsafeDiscardStringContext committed.config.systemd.units."burrow-fresh-runner@.service".text))));
  freshGot = lib.mapAttrs (name: want: builtins.filter (l: builtins.elem l (freshSections.${name} or [ ])) want) freshWant;
  handled = committed.config.burrow.events.handlers.dispatch.repos;
  # A runner with its `.runner` is checked against GitHub's list, and one
  # gone from it is stopped with the path unit that starts it again: that
  # path unit, and none of a runner not checked.
  registrations = lib.mapAttrs (_: r: r.registration) (settings committed).runners;
  polkit = committed.config.security.polkit.extraConfig;
in
  assert lib.assertMsg ((settings privateRepos).private == { owners = [ "sam" ]; app = 123; gh = "${pkgs.gh}/bin/gh"; }
    && privateRepos.config.burrow.events.handlers.dispatch.repos == null
    && builtins.elem "repository" privateRepos.config.burrow.events.handlers.dispatch.events
    && privateRepos.config.systemd.services.burrow-dispatch.serviceConfig.LoadCredential == [ "app-key:/run/secrets/steward" ])
    "dispatch: private repositories need owner discovery, events and the steward credential";
  assert lib.assertMsg (freshUnit.serviceConfig.ProtectHome
    && builtins.elem "-/data" freshUnit.serviceConfig.InaccessiblePaths)
    "dispatch: runners must not see host homes or data";
  assert lib.assertMsg (deploy == want) "dispatch: ${builtins.toJSON deploy}, not ${builtins.toJSON want}";
  assert lib.assertMsg (settings dirty == settings committed)
    "dispatch: a build from a dirty tree isn't the same tree committed: ${builtins.toJSON (settings dirty)}, ${builtins.toJSON (settings committed)}";
  # What HOST-update tells main from local changes by: the switch stamps it.
  assert lib.assertMsg (revisions == [ "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dirty" ])
    "dispatch: the systems are stamped ${builtins.toJSON revisions}";
  assert lib.assertMsg (registrations == { ci-runner = "/var/lib/ci/runner/.runner"; bare = null; })
    "dispatch: the runners' registrations are ${builtins.toJSON registrations}";
  assert lib.assertMsg (lib.hasInfix ''var paths = ["ci-runner.path"];'' polkit)
    "dispatch: it may stop the path units ${polkit}";
  assert lib.assertMsg (labels committed == wantLabels "X64") "dispatch: x86_64's labels: ${builtins.toJSON (labels committed)}";
  assert lib.assertMsg (labels arm == wantLabels "ARM64") "dispatch: aarch64's labels: ${builtins.toJSON (labels arm)}";
  assert lib.assertMsg ((settings committed).fresh == wantFresh) "dispatch: fresh runners ${builtins.toJSON (settings committed).fresh}";
  assert lib.assertMsg (builtins.elem "sam-org/homelab" handled) "dispatch: the fresh runners' repo's events aren't its: ${builtins.toJSON handled}";
  assert lib.assertMsg (freshGot == freshWant) "dispatch: a fresh runner's unit has ${builtins.toJSON freshGot}";
  # From the host's mount namespace: from the unit's, it can't
  # (tests/fresh-runner.nix boots one).
  assert lib.assertMsg (lib.hasInfix "nsenter -t 1 -m -- " freshUnit.serviceConfig.ExecStopPost
    && lib.hasSuffix " /run/burrow-fresh/%i /var/lib/private/burrow-fresh-runner/%i /var/lib/burrow-fresh-runner/%i" freshUnit.serviceConfig.ExecStopPost)
    "dispatch: a fresh runner's registration or state outlives it: ${freshUnit.serviceConfig.ExecStopPost}";
  assert lib.assertMsg (lib.hasInfix "nsenter -t 1 -m -- " freshUnit.serviceConfig.ExecStartPost
    && lib.hasSuffix "rm -rf /run/burrow-fresh/%i" freshUnit.serviceConfig.ExecStartPost)
    "dispatch: a fresh runner's registration outlives its start: ${freshUnit.serviceConfig.ExecStartPost}";
  assert lib.assertMsg (!freshUnit.restartIfChanged && !freshUnit.stopIfChanged) "dispatch: a switch can cut a fresh runner's job short";
  # The job they're for, burrow's flake-check.yml, calls these.
  assert lib.assertMsg (lib.all (p: builtins.elem p (map lib.getName freshUnit.path)) [ "jq" "gh" "curl" ])
    "dispatch: a fresh runner's jobs find ${toString (map lib.getName freshUnit.path)} on PATH";
  assert lib.assertMsg ((settings dispatchOnly).fresh == null) "dispatch: fresh runners nobody asked for";
  # What tells Conary which runners' CI it counts.
  assert lib.assertMsg ((committed.config.burrow.ledger.env.CONARY_FRESH_RUNNERS or null) == "fresh-burrow-"
    && !(dispatchOnly.config.burrow.ledger.env ? CONARY_FRESH_RUNNERS))
    "dispatch: the ledger's cn is told fresh runners' names start ${builtins.toJSON (committed.config.burrow.ledger.env.CONARY_FRESH_RUNNERS or null)}, and ${builtins.toJSON (dispatchOnly.config.burrow.ledger.env.CONARY_FRESH_RUNNERS or null)} with none";
  assert lib.assertMsg (map (a: a.message) (refused overKept) == [ "burrow.dispatch.fresh.idle (3) is more than there may be at once (most, 2)." ]
    && refused committed == [ ])
    "dispatch: more fresh runners kept waiting than there may be refuses ${builtins.toJSON (map (a: a.message) (refused overKept))}";
  assert lib.assertMsg (settings unhandled == settings committed)
    "dispatch: with the handlers off, the settings are ${builtins.toJSON (settings unhandled)}";
  assert lib.assertMsg ((settings committed).gap == 120 && (settings spaced).gap == 600)
    "dispatch: runners' waits allow for a ${toString (settings committed).gap}s gap, and ${toString (settings spaced).gap}s with passes 600s apart";
  pkgs.runCommand "dispatch-config-check" { } ''
    ${self.nixosConfigurations.example.config.systemd.services.burrow-dispatch.serviceConfig.ExecStart} status
    ${committed.config.systemd.services.burrow-dispatch.serviceConfig.ExecStart} status
    touch $out
  ''
