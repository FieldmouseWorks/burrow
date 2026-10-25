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
      names = [ "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "toolbox" "autoland" "house" ];
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
        # check, so these build nothing.
        standalone = pkgs.writeText "burrow-standalone" (nixpkgs.lib.concatMapStrings (name:
          let
            machine = nixpkgs.lib.nixosSystem {
              modules = [
                self.nixosModules.${name}
                ./examples/machine.nix
                (nixpkgs.lib.optionalAttrs (name == "robot") { burrow.robot.serverIp = "203.0.113.7"; })
                (nixpkgs.lib.optionalAttrs (name == "undo") { burrow.undo.datasets = [ "rpool/safe/data" ]; })
                (nixpkgs.lib.optionalAttrs (name == "updates") { burrow.updates.origin = "https://github.com/sam/homelab"; })
                (nixpkgs.lib.optionalAttrs (name == "autoland") { burrow.autoland = { owners = [ "sam" ]; authors = [ "sam" ]; deepseek = { enable = true; envFile = "/run/secrets/deepseek-review"; }; }; })
                (nixpkgs.lib.optionalAttrs (name == "house") { burrow.house.owners = [ "sam" ]; })
                { burrow.${if name == "remote-control" then "remoteControl" else name}.enable = true; }
              ];
            };
          in "${name} ${builtins.unsafeDiscardStringContext machine.config.system.build.toplevel.drvPath}\n") names);
        # Sandboxes for real: boot two, work in them, break out of nothing.
        # Nested KVM: run it on a machine that has /dev/kvm.
        agents = import ./tests/agents.nix { inherit self pkgs; };
        # proj's queue and memory caps, with a stand-in for Claude.
        proj = import ./tests/proj.nix { inherit self pkgs; };
        # The steward's DeepSeek reviewer, with stand-ins for gh and the API,
        # and its one-way paths as the module sets them.
        autoland = pkgs.runCommand "autoland-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git ]; } ''
          python3 -I ${./tests/autoland}/check.py ${./modules/autoland}/steward.py \
            ${pkgs.writeText "one-way.json" (builtins.toJSON self.nixosConfigurations.example.config.burrow.autoland.oneWay)}
          touch $out
        '';
        # Which Renovate PRs renovate-sweep hands over and lists, with stand-ins for gh and proj.
        renovate-sweep = pkgs.runCommand "renovate-sweep-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.jq pkgs.gnused ]; } ''
          python3 -I ${./tests/renovate-sweep.py} ${./modules/updates/renovate-sweep.sh}
          touch $out
        '';
        # Stale Remote Control servers catch up once idle, with a stand-in for Claude.
        remote-control = import ./tests/remote-control.nix { inherit self pkgs; };
      } // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "house-${name}") (
        # lib/project.nix's own gates, on the smallest projects that have them.
        (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust = { }; }).checks.${system}
        // (self.lib.project { self = ./tests/house/python; systems = [ system ]; python = { format = true; }; }).checks.${system}
      );

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
