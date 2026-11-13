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

      # Each module gets burrow's own inputs (the pinned Claude Code, microvm.nix),
      # a key, so importing one twice (say, on its own and via default) is
      # harmless, and its file, so what it defines says where it came from
      # (in an error, and to update-origin) rather than nixpkgs' flake.nix.
      module = name: {
        _file = toString ./modules/${name}.nix;
        key = "burrow/${name}";
        imports = [ (import ./modules/${name}.nix inputs) ];
      };
      # The ledger's machines take Conary from its own flake; burrow's checks
      # build theirs with a stand-in for its `cn`, which passes the policy
      # file their build checks (`cn policy`) and nothing else.
      cnStandIn = pkgs.writeShellScriptBin "cn" ''[ "$1" = policy ] && exit 0; exit 1'';
      names = [ "settings" "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "probe" "toolbox" "autoland" "house" "events" "ledger" "browser" "heartbeat" "backup" "watchdog" "github" "rust-cache" "cpu" "dispatch" "usage-reset" "dashboard" ];
      # Each module on its own, with only what it imports, in a check of its
      # own (standalone-NAME), so the gate's legs share the evaluations out:
      # evaluating is the check, so these build nothing. A service that says
      # how it runs (a Type) has something to run too: systemd refuses one
      # without ExecStart or ExecStop, and only when it starts. A module that
      # registers a burrow-events handler has burrow-events to run it. And a
      # memory cap holds swap to it too: a unit that may swap outgrows its
      # cap without ever reaching it, and the machine thrashes rather than
      # the kernel killing inside the cap. The example is held to the same.
      runnable = name: machine: let
        sd = machine.config.systemd;
        s = sd.services;
        empty = builtins.filter (n: s.${n}.enable && s.${n}.serviceConfig ? Type
          && !(s.${n}.serviceConfig ? ExecStart || s.${n}.serviceConfig ? ExecStop)) (builtins.attrNames s);
        unrun = machine.config.burrow.events.handlers or { } != { } && !(s ? burrow-events);
        swapping = builtins.concatMap ({ kind, units, section }: builtins.concatMap (n:
          let c = units.${n}.${section}; in
          nixpkgs.lib.optional (toString (c.MemoryMax or "infinity") != "infinity"
            && toString (c.MemorySwapMax or "") != "0") "${kind} ${n}") (builtins.attrNames units)) [
          { kind = "service"; units = s; section = "serviceConfig"; }
          { kind = "slice"; units = sd.slices; section = "sliceConfig"; }
          { kind = "user service"; units = sd.user.services; section = "serviceConfig"; }
          { kind = "user slice"; units = sd.user.slices; section = "sliceConfig"; }
        ];
      in if empty != [ ] then throw "${name}: services with nothing to run: ${builtins.concatStringsSep ", " empty}"
         else if unrun then throw "${name}: burrow-events handlers with no burrow-events to start them"
         else if swapping != [ ] then throw "${name}: memory caps that let their units swap (give each MemorySwapMax = 0): ${builtins.concatStringsSep ", " swapping}"
         else machine;
      standalone = name: let
        machine = nixpkgs.lib.nixosSystem {
          modules = [
            self.nixosModules.${name}
            ./examples/machine.nix
            (nixpkgs.lib.optionalAttrs (name == "robot") { burrow.robot.serverIp = "203.0.113.7"; })
            (nixpkgs.lib.optionalAttrs (name == "undo") { burrow.undo.datasets = [ "rpool/safe/data" ]; })
            (nixpkgs.lib.optionalAttrs (name == "backup") { burrow.backup = { repository = "rclone:offsite:backups/burrow"; passwordFile = "/run/secrets/backup-password"; rcloneConfigFile = "/run/secrets/backup-rclone"; datasets = [ "rpool/safe/data" ]; }; })
            (nixpkgs.lib.optionalAttrs (name == "updates") { burrow.updates.origin = "https://github.com/sam/homelab"; })
            (nixpkgs.lib.optionalAttrs (name == "autoland") { burrow.autoland = { owners = [ "sam" ]; authors = [ "sam" ]; app = { id = 123456; accountId = 123457; slug = "sams-steward"; keyFile = "/run/secrets/autoland-app-key"; }; reviewers.codex.enable = true; }; burrow.ledger = { enable = true; package = cnStandIn; anchorRepo = "sam/homelab"; }; })
            (nixpkgs.lib.optionalAttrs (name == "house") { burrow.house.owners = [ "sam" ]; })
            (nixpkgs.lib.optionalAttrs (name == "dashboard") { burrow.dashboard = { host = "burrow.tail1234.ts.net"; owner = { id = 583231; login = "sam"; }; }; services.tailscale.enable = true; })
            (nixpkgs.lib.optionalAttrs (name == "probe") { burrow.probe = { repo = "sam/homelab"; authors = [ { login = "sam"; id = 1; } ]; }; })
            (nixpkgs.lib.optionalAttrs (name == "dispatch") { burrow.dispatch.runners.ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; }; })
            (nixpkgs.lib.optionalAttrs (name == "ledger") { burrow.ledger = { package = cnStandIn; owners = [ "sam" ]; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; sessionLinks = [ "sam/homelab" ]; daemon = true; }; })
            (nixpkgs.lib.optionalAttrs (builtins.elem name [ "events" "autoland" ]) { burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; }; })
            # github has no switch: it's on with an App to serve.
            (if name == "settings" then { }
             else if name == "github" then { burrow.github.apps.workers = { id = 2; accountId = 3; slug = "sam-workers"; keyFile = "/run/secrets/workers-key"; }; }
             else { burrow.${{ remote-control = "remoteControl"; rust-cache = "rustCache"; usage-reset = "usageReset"; }.${name} or name}.enable = true; })
          ];
        };
      in pkgs.writeText "burrow-standalone-${name}" "${builtins.unsafeDiscardStringContext (runnable name machine).config.system.build.toplevel.drvPath}\n";
      # burrow's own programs, one Cargo workspace (crates/): their package
      # and their gate, built for every platform lib/project.nix knows. Its
      # tests include the shared workflows' (crates/burrow-workflows), which
      # read these files and run git, jq and nix, and the gate's run its VM
      # tests in a slot of the real heavy (burrow-heavy); proj's clone repos
      # to read their origins, gh-token's make keys with openssl, lock-age's
      # check its wait against what the steward reads (lock_wait.py), and
      # house-sync's read burrow's own house blocks, burrow-settings' the
      # settings reference it writes and its house guidance, and attestation's ask a
      # token server of their own with curl. No test reads README.md,
      # AGENTS.md or CLAUDE.md: they're the docs lane's (.github/workflows/flake.yml).
      crates = self.lib.project {
        inherit self;
        rust = {
          dir = "crates";
          members = true;
          include = [
            ".github/workflows"
            ".github/flake-check"
            "templates/project/.github/workflows/flake.yml"
            "templates/project/.github/workflows/flake-retarget.yml"
            "modules/autoland/lock_wait.py"
            "modules/autoland/step_jobs.py"
            "house"
            "docs/settings-reference.md"
          ];
          testInputs = pkgs: [ pkgs.rustc pkgs.git pkgs.gh pkgs.jq pkgs.nix pkgs.util-linux pkgs.openssl pkgs.curl pkgs.python3 ];
        };
      };
      # A workspace below its repo's root whose test reads a file outside
      # it: lib/project.nix's `include`, for its checks.
      houseInclude = self.lib.project {
        self = ./tests/house/rust-include;
        systems = [ system ];
        rust = { dir = "ws"; include = [ "spec/fact.txt" ]; };
      };
      # A workspace whose build script finds a native library with
      # pkg-config: lib/project.nix's native inputs, for its checks.
      houseNative = {
        nativeBuildInputs = pkgs: [ pkgs.pkg-config ];
        buildInputs = pkgs: [ pkgs.zlib ];
      };
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

      # burrow's programs, each crate's a package of its own, which is what
      # a module runs, for each platform they're built for, and flake-gate,
      # for a machine to install. There's no package of them all: a module
      # that ran one from it would change with every crate.
      packages = nixpkgs.lib.foldl' nixpkgs.lib.recursiveUpdate { } [
        (nixpkgs.lib.mapAttrs (_: p: builtins.removeAttrs p [ "default" ]) crates.packages)
        { ${system}.flake-gate = pkgs.callPackage ./tools/flake-gate/package.nix { }; }
      ];

      # A bare machine with every module on, for the checks and as an example.
      nixosConfigurations.example = nixpkgs.lib.nixosSystem {
        modules = [ self.nixosModules.default ./examples/minimal.nix ];
      };

      # Each check is a file of its own, checks/NAME.nix, so a PR that adds
      # one touches no file another PR does: when they were all here, this
      # was the file burrow's PRs conflicted on most. A check's name is its
      # file's, read without evaluating it, so the gate's legs can still
      # evaluate each one apart. The ones a function makes many of follow.
      checks.${system} =
        let
          files = nixpkgs.lib.filterAttrs (file: type: type == "regular" && nixpkgs.lib.hasSuffix ".nix" file) (builtins.readDir ./checks);
          args = { inherit self pkgs nixpkgs inputs system crates houseInclude houseNative runnable standalone cnStandIn; };
          own = nixpkgs.lib.mapAttrs' (file: _: nixpkgs.lib.nameValuePair (nixpkgs.lib.removeSuffix ".nix" file) (import ./checks/${file} args)) files;
          made =
            # Renovate's setup (tests/renovate-checks.nix).
            import ./tests/renovate-checks.nix { inherit self pkgs; }
            # lib/project.nix's rust.members: each program a package of its own.
            // import ./tests/house-members.nix { inherit self pkgs; }
            // nixpkgs.lib.listToAttrs (map (name: nixpkgs.lib.nameValuePair "standalone-${name}" (standalone name)) names)
            // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "crates-${name}") crates.checks.${system}
            // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "house-${name}") (
              # lib/project.nix's own gates, on the smallest projects that have them.
              (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust.testInputs = pkgs: [ pkgs.git ]; }).checks.${system}
              // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "native-${name}")
                (self.lib.project { self = ./tests/house/rust-native; systems = [ system ]; rust = houseNative; }).checks.${system}
              // (self.lib.project { self = ./tests/house/python; systems = [ system ]; python = { format = true; }; }).checks.${system}
              // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "include-${name}") houseInclude.checks.${system}
            );
          twice = builtins.attrNames (builtins.intersectAttrs own made);
        in
        if twice != [ ] then throw "checks/ has a file named for a check that's made below it too: ${toString twice}"
        else own // made;

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
