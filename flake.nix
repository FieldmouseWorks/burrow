{
  description = "burrow: NixOS modules for a box you run by talking to Claude";

  # Inputs are git+https rather than github: so they also fetch from sandboxes
  # that only allow plain git reads of GitHub.
  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-26.05&shallow=1";
    # Claude Code moves fast; it tracks unstable on its own.
    nixpkgs-unstable.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable&shallow=1";
    # The agent sandboxes (modules/agents.nix).
    microvm = {
      url = "git+https://github.com/microvm-nix/microvm.nix?shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Projects' toolchains and gates (lib/project.nix).
    rust-overlay = {
      url = "git+https://github.com/oxalica/rust-overlay?shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "git+https://github.com/ipetkov/crane?shallow=1";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, ... }@inputs:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};

      # Each module gets burrow's own inputs (the pinned Claude Code, microvm.nix)
      # and a key, so importing one twice (say, on its own and via default) is
      # harmless.
      module = name: {
        key = "burrow/${name}";
        imports = [ (import ./modules/${name}.nix inputs) ];
      };
      # The ledger's machines take Conary from its own flake; burrow's checks
      # build theirs with a stand-in for its `cn`.
      cnStandIn = pkgs.writeShellScriptBin "cn" "exit 1";
      names = [ "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "toolbox" "autoland" "house" "events" "ledger" "browser" ];
    in
    {
      # A project's dev shell and gate in a few lines; see lib/project.nix.
      lib.project = import ./lib/project.nix { inherit nixpkgs rust-overlay crane; };

      templates.default = {
        path = ./templates/project;
        description = "A project whose gate is its flake (burrow.lib.project)";
      };

      nixosModules = nixpkgs.lib.genAttrs names module // {
        default = {
          key = "burrow/default";
          imports = map module names;
        };
      };

      # A bare machine with every module on, for the checks and as an example.
      nixosConfigurations.example = nixpkgs.lib.nixosSystem {
        modules = [ self.nixosModules.default ./examples/minimal.nix ];
      };

      checks.${system} = {
        example = self.nixosConfigurations.example.config.system.build.toplevel;
        # Each module on its own, with only what it imports: evaluating is the
        # check, so these build nothing. A service that says how it runs (a
        # Type) has something to run too: systemd refuses one without
        # ExecStart or ExecStop, and only when it starts. A module that
        # registers a burrow-events handler has burrow-events to run it.
        standalone = let
          runnable = name: machine: let
            s = machine.config.systemd.services;
            empty = builtins.filter (n: s.${n}.enable && s.${n}.serviceConfig ? Type
              && !(s.${n}.serviceConfig ? ExecStart || s.${n}.serviceConfig ? ExecStop)) (builtins.attrNames s);
            unrun = machine.config.burrow.events.handlers or { } != { } && !(s ? burrow-events);
          in if empty != [ ] then throw "${name}: services with nothing to run: ${builtins.concatStringsSep ", " empty}"
             else if unrun then throw "${name}: burrow-events handlers with no burrow-events to start them"
             else machine;
        in pkgs.writeText "burrow-standalone" (nixpkgs.lib.concatMapStrings (name:
          let
            machine = nixpkgs.lib.nixosSystem {
              modules = [
                self.nixosModules.${name}
                ./examples/machine.nix
                (nixpkgs.lib.optionalAttrs (name == "robot") { burrow.robot.serverIp = "203.0.113.7"; })
                (nixpkgs.lib.optionalAttrs (name == "undo") { burrow.undo.datasets = [ "rpool/safe/data" ]; })
                (nixpkgs.lib.optionalAttrs (name == "updates") { burrow.updates.origin = "https://github.com/sam/homelab"; })
                (nixpkgs.lib.optionalAttrs (name == "autoland") { burrow.autoland = { owners = [ "sam" ]; authors = [ "sam" ]; reviewers = { codex.enable = true; deepseek = { enable = true; envFile = "/run/secrets/deepseek-review"; }; }; }; })
                (nixpkgs.lib.optionalAttrs (name == "house") { burrow.house.owners = [ "sam" ]; })
                (nixpkgs.lib.optionalAttrs (name == "ledger") { burrow.ledger = { package = cnStandIn; owners = [ "sam" ]; }; })
                (nixpkgs.lib.optionalAttrs (builtins.elem name [ "events" "autoland" ]) { burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; }; })
                { burrow.${if name == "remote-control" then "remoteControl" else name}.enable = true; }
              ];
            };
          in "${name} ${builtins.unsafeDiscardStringContext (runnable name machine).config.system.build.toplevel.drvPath}\n")
            names + "example ${builtins.unsafeDiscardStringContext (runnable "example" self.nixosConfigurations.example).config.system.build.toplevel.drvPath}\n");
        # Sandboxes for real: boot two, work in them, break out of nothing.
        # Nested KVM: run it on a machine that has /dev/kvm.
        agents = import ./tests/agents.nix { inherit self pkgs; };
        # proj's queue and memory caps, with a stand-in for Claude.
        proj = import ./tests/proj.nix { inherit self pkgs; };
        # The steward and its reviewers, with stand-ins for gh, git's remote and
        # the agents, and its one-way paths as the module sets them.
        autoland = pkgs.runCommand "autoland-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.openssl ]; } ''
          python3 -I ${./tests/autoland}/check.py ${./modules/autoland}/steward.py \
            ${pkgs.writeText "one-way.json" (builtins.toJSON self.nixosConfigurations.example.config.burrow.autoland.oneWay)}
          touch $out
        '';
        # The steward on a machine without /bin/sh: its sandbox's store still
        # builds.
        autoland-shellless = let
          machine = nixpkgs.lib.nixosSystem {
            modules = [
              self.nixosModules.autoland
              ./examples/machine.nix
              ({ lib, ... }: {
                burrow.autoland = { enable = true; owners = [ "sam" ]; authors = [ "sam" ]; reviewers.codex.enable = true; };
                burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; };
                environment.binsh = lib.mkForce null;
              })
            ];
          };
        in builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward") machine.config.environment.systemPackages);
        # burrow-events for real: deliveries, kicks and wake-ups starting a
        # handler's units through polkit.
        events-service = import ./tests/events.nix { inherit self pkgs; };
        # Which deliveries and kicks start whose pass, with a stand-in for systemctl.
        events = pkgs.runCommand "events-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
          python3 -I ${./tests/events.py} ${./modules/events}/events.py
          touch $out
        '';
        # Which repos a run of Renovate on one repo covers: the ones its
        # autodiscoverFilter takes, as Renovate's own code has it.
        renovate-covers = pkgs.runCommand "renovate-covers-check" { } ''
          # A /regex/ minds case: Sam/Secret is out, sam/secret isn't (so
          # renovate@ asks with GitHub's spelling).
          covers() { ${pkgs.lib.getExe pkgs.nodejs_24} ${./modules/updates/renovate-covers.mjs} ${pkgs.renovate} '["sam/*", "Kim/r", "!sam/{old,archive}", "!/^sam\/tmp-/", "!/^Sam\/Secret$/"]' "$1"; }
          for r in sam/r SAM/X kim/r sam/secret; do covers "$r" || { echo "$r: not covered"; exit 1; }; done
          for r in sam/old sam/archive sam/tmp-1 Sam/Secret kim/q other/r; do
            rc=0; covers "$r" || rc=$?
            [ "$rc" = 10 ] || { echo "$r: $rc, not 10"; exit 1; }
          done
          touch $out
        '';
        # Which Renovate PRs renovate-sweep hands over and lists, with stand-ins for gh and proj.
        renovate-sweep = pkgs.runCommand "renovate-sweep-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.jq pkgs.gnused ]; } ''
          python3 -I ${./tests/renovate-sweep.py} ${./modules/updates/renovate-sweep.sh} ${./modules/updates/major-seal.py} ${./modules/autoland/seal.py}
          touch $out
        '';
        # Which repos house-sync passes over, and when it tells the owner, with
        # stand-ins for gh and the notifier.
        house-sync = pkgs.runCommand "house-sync-check" { nativeBuildInputs = [ pkgs.python3 pkgs.jq pkgs.util-linux pkgs.diffutils ]; } ''
          python3 -I ${./tests/house-sync.py} ${./modules/house/sync.sh} ${./house/rules.md}
          touch $out
        '';
        # Stale Remote Control servers catch up once idle, with a stand-in for Claude.
        remote-control = import ./tests/remote-control.nix { inherit self pkgs; };
        # The signed-in browser: Claude driving it, its logins kept, its ports
        # and profile kept from everyone else.
        browser = import ./tests/browser.nix { inherit self pkgs; };
        # lib/project.nix's package of a Rust workspace: every member's binaries.
        house-rust-package = let
          pkg = (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
        in pkgs.runCommand "house-rust-package-check" { } ''
          [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
          touch $out
        '';
      } // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "house-${name}") (
        # lib/project.nix's own gates, on the smallest projects that have them.
        (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust = { }; }).checks.${system}
        // (self.lib.project { self = ./tests/house/python; systems = [ system ]; python = { format = true; }; }).checks.${system}
      );

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
