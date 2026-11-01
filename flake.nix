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
      # build theirs with a stand-in for its `cn`.
      cnStandIn = pkgs.writeShellScriptBin "cn" "exit 1";
      names = [ "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "probe" "toolbox" "autoland" "house" "events" "ledger" "browser" "heartbeat" "backup" "watchdog" "github" "rust-cache" "cpu" "dispatch" "usage-reset" ];
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
            (nixpkgs.lib.optionalAttrs (name == "autoland") { burrow.autoland = { owners = [ "sam" ]; authors = [ "sam" ]; app = { id = 123456; slug = "sams-steward"; keyFile = "/run/secrets/autoland-app-key"; }; reviewers.codex.enable = true; }; })
            (nixpkgs.lib.optionalAttrs (name == "house") { burrow.house.owners = [ "sam" ]; })
            (nixpkgs.lib.optionalAttrs (name == "probe") { burrow.probe = { repo = "sam/homelab"; authors = [ "sam" ]; }; })
            (nixpkgs.lib.optionalAttrs (name == "dispatch") { burrow.dispatch.runners.ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; }; })
            (nixpkgs.lib.optionalAttrs (name == "ledger") { burrow.ledger = { package = cnStandIn; owners = [ "sam" ]; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; sessionLinks = [ "sam/homelab" ]; daemon = true; }; })
            (nixpkgs.lib.optionalAttrs (builtins.elem name [ "events" "autoland" ]) { burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; }; })
            # github has no switch: it's on with an App to serve.
            (if name == "github" then { burrow.github.apps.workers = { id = 2; slug = "sam-workers"; keyFile = "/run/secrets/workers-key"; }; }
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
      # house-sync's read burrow's own house blocks.
      crates = self.lib.project {
        inherit self;
        rust = {
          dir = "crates";
          members = true;
          include = [
            ".github/workflows/flake-check.yml"
            ".github/workflows/flake.yml"
            ".github/workflows/flake-retarget.yml"
            "templates/project/.github/workflows/flake.yml"
            "templates/project/.github/workflows/flake-retarget.yml"
            "modules/autoland/prs.py"
            "modules/autoland/lock_wait.py"
            "house"
          ];
          testInputs = pkgs: [ pkgs.git pkgs.jq pkgs.nix pkgs.util-linux pkgs.openssl ];
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

      checks.${system} = {
        # The example, built, and runnable as each module on its own is.
        example = (runnable "example" self.nixosConfigurations.example).config.system.build.toplevel;
        # lib/project.nix's secrets check over all of burrow's tracked files;
        # the projects below each see only their own directory.
        secrets = (self.lib.project { inherit self; systems = [ system ]; }).checks.${system}.secrets;
        # heavy's slots: one holder at a time per slot, its share of the
        # threads, nested calls let through, the exit status and stdin passed
        # on, a cancelled workload stopped before its slot goes, a bounded
        # wait refused on time; and the example's gate slice weighted.
        cpu = let
          heavy = (self.nixosConfigurations.example.extendModules {
            modules = [{ burrow.cpu.heavy = { slots = 1; grace = 3; dir = "/build/heavy"; }; }];
          }).config.burrow.tools.heavy;
          weight = self.nixosConfigurations.example.config.systemd.slices.ci.sliceConfig.CPUWeight;
          # A gate slice with a dash in its name would be nested: refused.
          nested = (builtins.tryEval (builtins.deepSeq
            (self.nixosConfigurations.example.extendModules { modules = [{ burrow.cpu.gates.slices = [ "nix-builds" ]; }]; }).config.burrow.cpu.gates.slices
            true)).success;
        in pkgs.runCommand "cpu-check" { } ''
          [ ${toString weight} = 400 ] || { echo "ci.slice weight ${toString weight}"; exit 1; }
          ${pkgs.lib.optionalString nested "echo 'a nested gate slice (nix-builds) was taken'; exit 1"}
          mkdir -p /build/heavy && : >/build/heavy/slot1 && chmod 0444 /build/heavy/slot1
          h=${heavy}/bin/heavy
          threads=$(nproc)
          # Waits until COMMAND succeeds, for what process PID makes ready, and
          # fails if PID exits first or it takes over 30 seconds.
          ready() {
            local pid=$1 n=0; shift
            until "$@" 2>/dev/null; do
              kill -0 "$pid" 2>/dev/null || { echo "pid $pid exited before $*"; exit 1; }
              n=$(( n + 1 )); [ "$n" -lt 3000 ] || { echo "not $* after 30s"; exit 1; }
              sleep 0.01
            done
          }
          [ "$($h -- printenv HEAVY_JOBS)" = "$threads" ] || { echo "share: not $threads"; exit 1; }
          [ "$(CARGO_BUILD_JOBS=3 $h -- printenv CARGO_BUILD_JOBS)" = 3 ] || { echo "overrode CARGO_BUILD_JOBS"; exit 1; }
          rc=0; $h -- sh -c 'exit 7' || rc=$?
          [ "$rc" = 7 ] || { echo "exit status $rc, not 7"; exit 1; }
          # Inside the slot, a nested call runs at once; a second holder waits, and gives up when told to.
          $h -- sh -c "touch held; while [ ! -e done ]; do sleep 0.1; done" &
          ready $! test -e held
          rc=0; $h --wait 1 -- true 2>err || rc=$?
          [ "$rc" = 75 ] || { echo "a second holder got in ($rc)"; exit 1; }
          HEAVY_SLOT=1 $h --wait 1 -- true || { echo "a nested call waited"; exit 1; }
          $h status | grep -q '^slot1: busy' || { echo "status: $($h status)"; exit 1; }
          touch done; wait
          $h status | grep -q '^slot1: free' || { echo "status: $($h status)"; exit 1; }

          # The caller's stdin reaches the command: a pipe, a file, a terminal, and nested.
          rc=0; printf 'exit 7\n' | $h -- sh || rc=$?
          [ "$rc" = 7 ] || { echo "piped stdin: $rc, not 7"; exit 1; }
          echo 'exit 8' >script; rc=0; $h -- sh <script || rc=$?
          [ "$rc" = 8 ] || { echo "redirected stdin: $rc, not 8"; exit 1; }
          rc=0; printf 'exit 9\n' | HEAVY_SLOT=1 $h -- sh || rc=$?
          [ "$rc" = 9 ] || { echo "nested stdin: $rc, not 9"; exit 1; }
          rc=0; printf '6\n' | ${pkgs.util-linux}/bin/script -qec "$h -- sh -c 'test -t 0 || exit 1; read n; exit \$n'" /dev/null >/dev/null || rc=$?
          [ "$rc" = 6 ] || { echo "terminal stdin: $rc, not 6"; exit 1; }

          # Cancelled, heavy stops the whole workload, a shell's worker too,
          # and holds the slot until the last of it has gone. (Background
          # jobs here ignore INT: env gives it back.) Each worker names
          # itself once its traps are set, so the signal never comes first.
          for sig in TERM INT HUP; do
            rm -f worker stopped
            env --default-signal=INT $h -- sh -c 'sh -c "trap \"sleep 1; touch stopped; exit\" TERM; echo \$\$ >worker; while :; do sleep 0.1; done" & wait' &
            hp=$!
            ready $hp test -s worker
            kill -$sig $hp; wait $hp || true
            [ -e stopped ] || { echo "$sig: the slot went before the worker stopped"; exit 1; }
            ! kill -0 "$(cat worker)" 2>/dev/null || { echo "$sig: the worker outlived heavy"; exit 1; }
            $h status | grep -q '^slot1: free' || { echo "$sig: status: $($h status)"; exit 1; }
          done
          # A command that ignores the signal, or hangs in its handler with a
          # worker that ignores everything, is killed GRACE (3) seconds after
          # the signal, worker too, and only then does the slot go.
          for sig in TERM INT HUP; do
            for cmd in \
              'trap "" TERM INT HUP; echo $$ >worker; while :; do sleep 0.1; done' \
              'trap "sleep 60" TERM INT HUP; sh -c "trap \"\" TERM INT HUP; echo \$\$ >worker; while :; do sleep 0.1; done" & wait'; do
              rm -f worker
              env --default-signal=INT $h -- sh -c "$cmd" & hp=$!
              ready $hp test -s worker
              s=$(date +%s%N); kill -$sig $hp; wait $hp || true; e=$(date +%s%N)
              ms=$(( (e - s) / 1000000 ))
              [ "$ms" -ge 2900 ] && [ "$ms" -lt 6000 ] || { echo "$sig, stubborn command: heavy took ''${ms}ms, not about 3s"; exit 1; }
              ! kill -0 "$(cat worker)" 2>/dev/null || { echo "$sig: a stubborn command outlived heavy"; exit 1; }
              $h status | grep -q '^slot1: free' || { echo "$sig, stubborn: status: $($h status)"; exit 1; }
            done
          done
          # Cancelled as it starts, nothing of the command runs on. The
          # signal goes to heavy itself: sent before the fork has become
          # heavy, it reaches only the test's shell, which in the sandbox, under
          # load, can lose it.
          for i in $(seq 10); do
            $h -- sh -c 'sleep 1; touch late' & hp=$!
            ready $hp grep -qF "$h" /proc/$hp/cmdline
            kill -TERM $hp; wait $hp || true
            sleep 1.2
            [ ! -e late ] || { echo "the command outlived a heavy cancelled at its start"; exit 1; }
          done 2>/dev/null

          # A bounded wait ends on time, and never starts the command after,
          # once the slot is free. The slot is held until the waits are done,
          # not for a time: on a loaded runner, starting heavy twice can take
          # longer than any margin.
          rm -f ran held done
          $h -- sh -c 'touch held; while [ ! -e done ]; do sleep 0.1; done' & hp=$!
          ready $hp test -e held
          s=$(date +%s%N); rc=0; $h --wait 1 -- touch ran 2>err || rc=$?; e=$(date +%s%N)
          [ "$rc" = 75 ] || { echo "--wait 1 with the slot held: $rc, not 75"; exit 1; }
          [ $(( (e - s) / 1000000 )) -lt 1800 ] || { echo "--wait 1 took $(( (e - s) / 1000000 ))ms"; exit 1; }
          rc=0; $h --wait 0 -- touch ran 2>err || rc=$?
          [ "$rc" = 75 ] || { echo "--wait 0 with the slot held: $rc, not 75"; exit 1; }
          touch done; wait $hp
          sleep 0.5
          [ ! -e ran ] || { echo "a bounded wait ran the command once the slot was free"; exit 1; }
          $h -- touch ran 2>err
          [ -e ran ] || { echo "an unbounded wait didn't get the slot"; exit 1; }
          wait
          touch $out
        '';
        # Sandboxes for real: boot two, work in them, break out of nothing.
        # Nested KVM: run it on a machine that has /dev/kvm.
        agents = import ./tests/agents.nix { inherit self pkgs; };
        # proj's turns and memory caps, with a stand-in for Claude; a turn's
        # second run, once its first is stopped; who waits in line for its
        # slots; and its GitHub side, with a stand-in gh. Four tests on one
        # machine's config, so they run side by side.
        proj = import ./tests/proj.nix { inherit self pkgs; };
        proj-second-runs = import ./tests/proj-second-runs.nix { inherit self pkgs; };
        proj-slots = import ./tests/proj-slots.nix { inherit self pkgs; };
        proj-github = import ./tests/proj-github.nix { inherit self pkgs; };
        # A turn's clock, which stands still while it waits in line.
        proj-clock = import ./tests/proj-clock.nix { inherit self pkgs; };
        # A switch reaches the turns already waiting in line, each in its place.
        proj-deployed = import ./tests/proj-deployed.nix { inherit self pkgs; };
        # The example's proj reads the config its module writes (a field the
        # binary doesn't know fails it), and a job it doesn't have is failed;
        # so does its proj-inbox, with the inbox on, which has no issues yet.
        proj-config =
          let
            inbox = self.nixosConfigurations.example.extendModules {
              modules = [ { burrow.proj.inbox = { enable = true; authors = [ "octocat" ]; }; } ];
            };
          in
          pkgs.runCommand "proj-config-check" { } ''
            got=$(${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj state no-such-job)
            [ "$got" = failed ] || { echo "proj state said: $got"; exit 1; }
            got=$(${inbox.config.burrow.tools.proj-inbox}/bin/proj-inbox status)
            [ -z "$got" ] || { echo "proj-inbox status said: $got"; exit 1; }
            touch $out
          '';
        # The steward and its reviewers, with stand-ins for gh, git's remote and
        # the agents, and its one-way paths as the module sets them; a look it
        # queues is taken by the reviewers' own service, burrow-review, with
        # its tests' stand-ins for codex and bubblewrap (they need bash and jq).
        # Its passes run burrow-upkeep, whose source says what it lists, and
        # start a proof run through burrow-proof, which reads what they write.
        autoland = pkgs.runCommand "autoland-check" { nativeBuildInputs = [ pkgs.python3 pkgs.git pkgs.openssl pkgs.bash pkgs.jq ]; } ''
          python3 -I ${./tests/autoland}/check.py ${./modules/autoland}/steward.py \
            ${pkgs.writeText "one-way.json" (builtins.toJSON self.nixosConfigurations.example.config.burrow.autoland.oneWay)} \
            ${self.packages.${system}.burrow-review}/bin/burrow-review ${./crates/burrow-review/tests/fakes} \
            ${self.packages.${system}.burrow-upkeep}/bin/burrow-upkeep ${./crates/burrow-upkeep/src} \
            ${self.packages.${system}.burrow-proof}/bin/burrow-proof
          touch $out
        '';
        # The example's reviewers' service reads the config its module writes
        # (a field the binary doesn't know fails it).
        review-config = pkgs.runCommand "review-config-check" { } ''
          ${builtins.head (pkgs.lib.splitString " " self.nixosConfigurations.example.config.systemd.services.autoland-review.serviceConfig.ExecStart)} check
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
                burrow.autoland = { enable = true; owners = [ "sam" ]; authors = [ "sam" ]; app = { id = 123456; slug = "sams-steward"; keyFile = "/run/secrets/autoland-app-key"; }; reviewers.codex.enable = true; };
                burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; };
                environment.binsh = lib.mkForce null;
              })
            ];
          };
        in builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward") machine.config.environment.systemPackages);
        # The example's stall watchdog reads the config its module writes (a
        # field the binary doesn't know fails it).
        watchdog-config = pkgs.runCommand "watchdog-config-check" { } ''
          ${self.nixosConfigurations.example.config.systemd.services.burrow-watchdog.serviceConfig.ExecStart} status
          touch $out
        '';
        # The example's rustc wrapper reads the config its module writes: a
        # field the binary doesn't know is a line on stderr at every compile,
        # which goes uncached. Here, with no cache, it compiles without one
        # and says nothing.
        rustc-wrapper-config = pkgs.runCommand "rustc-wrapper-config-check" { } ''
          said=$(${self.nixosConfigurations.example.config.environment.etc."rust-cache/rustc-wrapper".source} ${pkgs.coreutils}/bin/echo compiled 2>&1)
          [ "$said" = compiled ] || { echo "the wrapper said: $said"; exit 1; }
          touch $out
        '';
        # The example's guard, as Claude Code runs it, reads the config its
        # module writes (a field the binary doesn't know makes it ask at
        # every command): a plain command goes through, a one-way door asks.
        guard-config = let
          guard = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.claude-guard;
          hook = cmd: builtins.toJSON { tool_name = "Bash"; tool_input.command = cmd; cwd = "/"; };
        in pkgs.runCommand "guard-config-check" { } ''
          export XDG_RUNTIME_DIR=$TMPDIR
          got=$(${guard} <<<${pkgs.lib.escapeShellArg (hook "ls")})
          [ -z "$got" ] || { echo "ls: $got"; exit 1; }
          got=$(${guard} <<<${pkgs.lib.escapeShellArg (hook "efibootmgr -o 0001")})
          ${pkgs.jq}/bin/jq -e '.hookSpecificOutput.permissionDecision == "ask"' <<<"$got"
          touch $out
        '';
        # Claude Code watches /etc/claude-code and follows what it finds: a
        # symlink there has it walking the Nix store for as long as it runs.
        claude-etc = let
          etc = self.nixosConfigurations.example.config.environment.etc;
          names = builtins.filter (pkgs.lib.hasPrefix "claude-code/") (builtins.attrNames etc);
          links = builtins.filter (name: etc.${name}.mode == "symlink") names;
        in pkgs.runCommand "claude-etc-check" { } ''
          ${pkgs.lib.optionalString (names == [ ]) ''echo "no claude-code files in /etc"; exit 1''}
          ${pkgs.lib.optionalString (links != [ ]) ''echo "symlinks into the store: ${toString links}"; exit 1''}
          touch $out
        '';
        # The steward's tools reach a pass however it's started: by hand or
        # a Claude job (`autoland-steward kick OWNER/REPO`), with no gh or
        # proj of its own, gets them from the wrapper, ahead of its own PATH. The reviewers' service gets none of them: only what its unit
        # gives it. Its gh is the machine's: in an App's env (a job's),
        # `status` and a project's clone go through the socket, and a pass's
        # own calls with the steward's token as given.
        autoland-path = let
          example = self.nixosConfigurations.example.config;
          steward = builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward")
            example.environment.systemPackages);
          # gh-token's side of the socket: the App's token for whatever's asked.
          socket = pkgs.writeText "fake-socket.py" ''
            import os, socket, sys
            s = socket.socket(socket.AF_UNIX)
            s.bind(sys.argv[1])
            s.listen()
            open(sys.argv[1] + ".up", "w").close()
            while True:
                c, _ = s.accept()
                c.recv(4096)
                c.sendall(b"token from-the-socket\n")
                c.close()
          '';
        in pkgs.runCommand "autoland-path-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
          w=${steward}/bin/autoland-steward
          # The wrapper, but showing the PATH it would run the steward with.
          grep -q '^exec .*steward.py' "$w" || { echo "autoland-steward doesn't exec the steward"; exit 1; }
          sed 's|^exec .*steward.py.*|printf "%s\\n" "$PATH"; exit|' "$w" >path-of
          for sub in kick status pass; do
            p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash path-of $sub)
            case $p in *:/caller) ;; *) echo "$sub: the caller's PATH isn't kept after the steward's: $p"; exit 1 ;; esac
            [ "$p" != /caller ] || { echo "autoland-steward sets no PATH of its own"; exit 1; }
            for t in gh git tar proj; do
              env -i PATH="$p" ${pkgs.bash}/bin/bash -c "command -v $t" >/dev/null || { echo "$sub: no $t on the steward's PATH"; exit 1; }
            done
          done
          r=${builtins.head (pkgs.lib.splitString " " example.systemd.services.autoland-review.serviceConfig.ExecStart)}
          grep -q '^exec .*/bin/burrow-review "$@"' "$r" || { echo "the reviewers' service isn't burrow-review"; exit 1; }
          sed 's|^exec .*|printf "%s\\n" "$PATH"; exit|' "$r" >review-path-of
          p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash review-path-of work)
          [ "$p" = /caller ] || { echo "review gets more than its units give it: $p"; exit 1; }
          p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash path-of status)
          gh=$(env -i PATH="$p" ${pkgs.bash}/bin/bash -c 'command -v gh')
          git=$(env -i PATH="$p" ${pkgs.bash}/bin/bash -c 'command -v git')
          [ "$gh" = ${example.burrow.tools.gh}/bin/gh ] || { echo "the steward's gh isn't the machine's: $gh"; exit 1; }
          [ "$git" = ${example.programs.git.package}/bin/git ] || { echo "the steward's git isn't the machine's: $git"; exit 1; }
          # As a job with an App runs it: no token of its own, the socket's.
          python3 -I ${socket} $PWD/app.sock & while [ ! -e app.sock.up ]; do sleep 0.1; done
          mkdir no-login
          ask() { env -i PATH="$p" HOME=$PWD GH_CONFIG_DIR=$PWD/no-login GH_TOKEN_SOCKET=$PWD/app.sock GH_TOKEN_OWNER=sam "$@" gh auth token 2>&1; }
          got=$(ask GH_TOKEN=) || true
          [ "$got" = from-the-socket ] || { echo "status in an App's env doesn't go through its socket: $got"; exit 1; }
          got=$(ask GH_TOKEN=the-stewards) || true
          [ "$got" = the-stewards ] || { echo "a pass's own call doesn't use the steward's token: $got"; exit 1; }
          touch $out
        '';
        # Each flaky the machine runs asks GitHub through the first gh on its
        # caller's PATH (the machine's, which knows an App's env), with no gh
        # of its own ahead of it: the toolbox's, and the steward's, which
        # burrow-upkeep runs as a library, reading the config the module
        # writes (a field it doesn't know fails it).
        flaky-gh = let
          example = self.nixosConfigurations.example.config;
          steward = builtins.head (builtins.filter (p: (p.name or "") == "autoland-steward")
            example.environment.systemPackages);
          # GitHub's answer to gh api -i for an empty quarantine, and a note
          # that it was asked.
          gh = pkgs.writeShellScript "gh" ''
            echo "$*" >>"$CALLED"
            printf 'HTTP/2.0 200 OK\nContent-Type: application/json\r\n\r\n[]'
          '';
        in pkgs.runCommand "flaky-gh-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
          mkdir bin && ln -s ${gh} bin/gh
          config=$(sed -n 's|^export AUTOLAND_CONFIG=.*:-\(/nix/store/[^}]*\)}$|\1|p' ${steward}/bin/autoland-steward)
          [ -n "$config" ] || { echo "autoland-steward names no config"; exit 1; }
          upkeep=$(jq -r '.upkeep | join(" ")' "$config")
          [ "$(jq -r '.flaky' "$(jq -r '.upkeep[1]' "$config")")" != null ] || { echo "the steward's upkeep has no flaky tests"; exit 1; }
          for flaky in "${example.system.path}/bin/flaky quarantine sam/r" "$upkeep watch sam/r --now 1800000000"; do
            rm -f called
            said=$(env -i PATH=$PWD/bin CALLED=$PWD/called $flaky 2>&1) || { echo "$flaky: $said"; exit 1; }
            grep -q '^api .*repos/sam/r/issues?labels=flaky' called || { echo "$flaky asked GitHub through a gh of its own: $said"; exit 1; }
          done
          touch $out
        '';
        # The dispatcher's config as the module writes it, read by the binary
        # (the example's, and one with a deploy to watch): which commit of
        # each watched input the running system has, from the flake's
        # flake.lock (tests/dispatch), with its own repo left to the
        # configuration revision the switch stamps, so that a build from a
        # dirty tree and the same tree committed are one system; and each
        # runner's labels,
        # GitHub's defaults for the machine's architecture among them unless
        # it has none.
        dispatch-config = let
          inherit (nixpkgs) lib;
          machine = own: modules: lib.nixosSystem {
            modules = [
              self.nixosModules.dispatch
              self.nixosModules.updates
              self.nixosModules.ledger
              ./examples/machine.nix
              {
                burrow.ledger = { package = cnStandIn; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; };
                burrow.dispatch = {
                  enable = true;
                  runners = {
                    ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; };
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
                  self = { outPath = ./tests/dispatch; } // own;
                  kit.rev = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
                  tools.rev = "cccccccccccccccccccccccccccccccccccccccc";
                  nixpkgs.rev = "dddddddddddddddddddddddddddddddddddddddd";
                };
              }
            ] ++ modules;
          };
          committed = machine { rev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; } [ ];
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
        in
          assert lib.assertMsg (deploy == want) "dispatch: ${builtins.toJSON deploy}, not ${builtins.toJSON want}";
          assert lib.assertMsg (settings dirty == settings committed)
            "dispatch: a build from a dirty tree isn't the same tree committed: ${builtins.toJSON (settings dirty)}, ${builtins.toJSON (settings committed)}";
          # What HOST-update tells main from local changes by: the switch stamps it.
          assert lib.assertMsg (revisions == [ "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dirty" ])
            "dispatch: the systems are stamped ${builtins.toJSON revisions}";
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
          '';
        # HOST-notify as the example machine has it: the config built into it
        # reads (a field the binary doesn't know fails it), and `pending`
        # answers without GitHub. Its rules are crates/burrow-notify's tests.
        notify-config = let
          notify = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.notify;
        in pkgs.runCommand "notify-config-check" { } ''
          code=0
          ${notify} pending nothing-asked || code=$?
          [ "$code" = 1 ] || { echo "pending: exit $code"; exit 1; }
          touch $out
        '';
        # robot as the example machine has it: the config built into it reads
        # (a field the binary doesn't know fails it), a call to a webservice
        # that isn't there says which call failed and nothing of the
        # credentials, and a call it can't read asks nothing. Its calls are
        # crates/burrow-robot's tests.
        robot-config = let
          robot = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.robot;
        in pkgs.runCommand "robot-config-check" { } ''
          code=0
          ROBOT_USER=sam ROBOT_PASSWORD=hunter2 ROBOT_URL=http://127.0.0.1:9 ${robot} wol 2>err || code=$?
          [ "$code" = 1 ] || { echo "wol: exit $code: $(cat err)"; exit 1; }
          grep -q '^robot: POST /wol/203.0.113.7: ' err || { echo "wol said: $(cat err)"; exit 1; }
          if grep -q hunter2 err; then echo "the password in: $(cat err)"; exit 1; fi
          code=0
          ${robot} reset cold 2>/dev/null || code=$?
          [ "$code" = 2 ] || { echo "reset cold: exit $code"; exit 1; }
          touch $out
        '';
        # claude-trust, as the machine gets it, on a stand-in ~/.claude.json:
        # both spellings of a folder, the rest kept, Claude Code's lock kept to.
        claude-trust = pkgs.runCommand "claude-trust-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.jq ]; } ''
          shellcheck -s bash ${./tests/claude-trust.sh}
          bash ${./tests/claude-trust.sh} ${pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.claude-trust}
          touch $out
        '';
        # The usage watcher's Claude Code starts in a folder its unit has
        # trusted already, so it never meets that question with nobody there:
        # under a state dir with a space in it too, one argument however it's
        # spelled (systemd's quoting, which bash reads the same for this one).
        usage-reset-trust = let
          example = (self.nixosConfigurations.example.extendModules {
            modules = [{ burrow.stateDir = pkgs.lib.mkForce "/var/lib/burrow state"; }];
          }).config;
          pre = map (pkgs.lib.removePrefix "-")
            (pkgs.lib.toList (example.systemd.services.burrow-usage-reset.serviceConfig.ExecStartPre or [ ]));
        in pkgs.runCommand "usage-reset-trust-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
          export HOME=$TMPDIR/home
          mkdir -p "$HOME"
          echo '{}' >"$HOME/.claude.json"
          ${pkgs.lib.concatLines pre}
          # Where the watcher starts it: its state dir's claude/.
          dir=${pkgs.lib.escapeShellArg "${example.burrow.stateDir}/usage-reset/claude"}
          jq -e --arg d "$dir" '.projects[$d].hasTrustDialogAccepted == true' "$HOME/.claude.json" >/dev/null ||
            { echo "burrow-usage-reset starts its Claude Code in $dir untrusted" >&2; exit 1; }
          touch $out
        '';
        # The update job's fix jobs: pipeline work, one per problem while it's
        # recent.
        update-dispatch = pkgs.runCommand "update-dispatch-check" { nativeBuildInputs = [ pkgs.shellcheck ]; } ''
          shellcheck -s bash ${./modules/updates/dispatch.sh} ${./tests/update-dispatch.sh}
          bash ${./tests/update-dispatch.sh} ${./modules/updates/dispatch.sh}
          touch $out
        '';
        # The update job's push: what went live reaches main, even when main moved
        # while it ran.
        update-push = pkgs.runCommand "update-push-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.git ]; } ''
          shellcheck -s bash ${./modules/updates/push.sh} ${./tests/update-push.sh}
          bash ${./tests/update-push.sh} ${./modules/updates/push.sh}
          touch $out
        '';
        # What HOST-update compares to tell what runs already
        # (modules/updates/running.sh): a system built again from a new commit
        # of the same tree runs already, and its record of what's live says
        # so. Real git, with a stand-in for nix.
        update-running = pkgs.runCommand "update-running-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.git pkgs.jq ]; } ''
          shellcheck -s bash ${./modules/updates/running.sh} ${./tests/update-running.sh}
          bash ${./tests/update-running.sh} ${./modules/updates/running.sh}
          touch $out
        '';
        # HOST-update's VM tests take a heavy slot.
        update-check = pkgs.runCommand "update-check-check" { nativeBuildInputs = [ pkgs.shellcheck ]; } ''
          shellcheck -s bash -e SC2154 ${./modules/updates/check.sh} ${./tests/update-check.sh}
          bash ${./tests/update-check.sh} ${./modules/updates/check.sh}
          touch $out
        '';
        # HOST-update's steps in Rust as the example machine builds them
        # (crates/burrow-update, whose tests are the rest): they read their
        # config, and its train's.
        update-config = let
          inherit (self.nixosConfigurations.example.config) burrow;
        in pkgs.runCommand "update-config-check" { } ''
          ${burrow.tools.update-steps}/bin/${burrow.prefix}-update-steps help 2>err && { echo "a usage went as a step"; exit 1; }
          grep -qx 'usage: ${burrow.prefix}-update-steps pr | .*' err || { echo "it said: $(cat err)"; exit 1; }
          touch $out
        '';
        # Which repo the flake's origin is, and so whether HOST-update goes
        # through its train: the same in every spelling of GitHub's URLs
        # (scheme, host case, a trailing dot, the default port, either SSH
        # form, .git, a trailing slash), whatever the case of autoland's name
        # for it; nothing for a repo elsewhere, even one with github.com in its
        # path; and a GitHub origin it can't read refused, never read as no
        # train.
        update-origin = let
          lib = nixpkgs.lib;
          # How the example takes an origin: through its train or not, and
          # refused or not by the updates module's own assertion. Only its
          # assertions are read: all 1,485 of the example's took 5 s an
          # origin, 139 s of one leg's gate.
          take = origin: let
            machine = self.nixosConfigurations.example.extendModules {
              modules = [{ burrow.updates.origin = lib.mkForce origin; }];
            };
            updates = lib.filter (d: lib.hasSuffix "/modules/updates.nix" (toString d.file)) machine.options.assertions.definitionsWithLocations;
          in {
            train = machine.config.burrow.updates.throughTrain;
            refused = lib.any (a: !a.assertion && lib.hasInfix "OWNER/NAME" a.message) (lib.concatMap (d: d.value) updates);
          };
          forms = [
            "https://github.com/sam/homelab" "https://github.com/sam/homelab.git" "https://github.com/sam/homelab/"
            "https://github.com/Sam/HomeLab" "https://GitHub.com/sam/homelab.git" "https://github.com./sam/homelab"
            "HTTPS://github.com:443/sam/homelab" "git@github.com:sam/homelab.git" "git@github.com:sam/homelab"
            "git@GITHUB.COM.:sam/homelab/" "ssh://git@github.com/sam/homelab.git" "ssh://git@github.com:22/sam/homelab/"
            "ssh://git@GitHub.com.:22/sam/homelab"
          ];
          wrong = lib.filter (o: let t = take o; in !t.train || t.refused) forms
            ++ lib.filter (o: let t = take o; in t.train || t.refused) [
              "https://git.example.org/sam/homelab" "https://github.com/sam/other" "/srv/mirrors/github.com/sam/homelab"
              "https://git.example.org/github.com/sam/homelab"
            ]
            ++ lib.filter (o: !(take o).refused) [
              "https://github.com/sam" "https://www.github.com/sam/homelab" "git@github.com:sam/homelab/extra"
              "https://github.com:8443/sam/homelab" "http://github.com/sam/homelab" "https://sam@github.com/sam/homelab"
              "ssh://sam@github.com/sam/homelab" "git://GitHub.com/sam/homelab"
            ];
        in pkgs.runCommand "update-origin-check" { } ''
          ${lib.concatMapStrings (o: "echo 'wrong for ${o}'
") wrong}
          [ ${toString (builtins.length wrong)} = 0 ]
          touch $out
        '';
        # A repo's settings in burrow.autoland.repos: each one the steward
        # reads is taken, and one it doesn't (a typo, or one from a newer
        # burrow) is refused rather than doing nothing. As update-origin
        # does, it reads only autoland.nix's assertions.
        autoland-repos = let
          lib = nixpkgs.lib;
          refused = repo: rc: let
            machine = self.nixosConfigurations.example.extendModules {
              modules = [{ burrow.autoland.repos.${repo} = rc; }];
            };
            mine = lib.filter (d: lib.hasSuffix "/modules/autoland.nix" (toString d.file)) machine.options.assertions.definitionsWithLocations;
          in lib.any (a: !a.assertion && lib.hasPrefix "burrow.autoland.repos: no such setting:" a.message
            && lib.hasInfix "\"${repo}\"." a.message) (lib.concatMap (d: d.value) mine);
          every = {
            rounds = 3; oneWay = [ "^deploy/" ]; handoffAfter = 30; sessionHandoffAfter = 30; settleDelay = 5;
            stallAfter = 60; holdStallAfter = 60; renovateMergeAfter = 60; directPush = false; ruleset = true;
            requireCi = false; proofJobs.hil = "Run the bench."; proofBuilds.vm = "checks.x86_64-linux.vm";
            proofRuns.gate = "make check"; proofSlots = 1; keepBranches = [ "keep/*" ]; codex = true;
            skipPaths = [ "^secret/" ]; train = false; trainSize = 8; trainRequire = [ ]; trainStallAfter = 60;
          };
          wrong = lib.filter (k: refused "sam/one" { ${k} = every.${k}; }) (lib.attrNames every)
            ++ map (k: "${k}, which is no setting") (lib.filter (k: !(refused "sam/typo" { ${k} = { }; }))
              [ "proofRun" "keepBranch" "Rounds" "proofjobs" "slots" ]);
        in pkgs.runCommand "autoland-repos-check" { } ''
          ${lib.concatMapStrings (w: "echo 'wrong for ${w}'
") wrong}
          [ ${toString (builtins.length wrong)} = 0 ]
          touch $out
        '';
        # burrow-events for real: deliveries, kicks and wake-ups starting a
        # handler's units through polkit.
        events-service = import ./tests/events.nix { inherit self pkgs; };
        # Probes answer on a private repo's issues, never read a unit off the
        # allowlist, and post a planted token scrubbed.
        probe = import ./tests/probe.nix { inherit self pkgs; };
        # A proof run in the unit the steward starts: each command fenced,
        # the whole run bounded, and all of it ended however it ends.
        proof-run = import ./tests/proof-run.nix { inherit self pkgs; };
        # HOST-switch's revert when the window runs out mid-activation.
        switch = import ./tests/switch.nix { inherit self pkgs; };
        # A runner started fresh for one job: its registration and state
        # gone however it ends, and a clean end not failed.
        fresh-runner = import ./tests/fresh-runner.nix { inherit self pkgs; };
        # systemd-oomd kills what eats the memory before the machine drowns.
        oomd = import ./tests/oomd.nix { inherit self pkgs; };
        # systemd pets the board's own watchdog, whichever came up first.
        hardware-watchdog = import ./tests/hardware-watchdog.nix { inherit self pkgs; };
        # HOST-health's ZFS check: the pools the machine needs, by name or
        # GUID, with a stand-in zpool.
        health-zfs = import ./tests/health-zfs.nix { inherit self pkgs; };
        # HOST-health as the example machine builds it (crates/burrow-health,
        # whose tests are the rest): it reads its config, and with no checks
        # and no systemd in the sandbox, reports nothing.
        health-config = let
          health = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.health;
        in pkgs.runCommand "health-config-check" { } ''
          got=$(${health} 2>err) || { echo "HOST-health failed (exit $?): $(cat err)"; exit 1; }
          [ -z "$got" ] || { echo "HOST-health reported: $got"; exit 1; }
          touch $out
        '';
        # heartbeat's beat as the example builds it, with a stand-in HOST-health.
        heartbeat = import ./tests/heartbeat.nix { inherit self pkgs; };
        # Off-site backups of ZFS snapshots through restic and rclone, and the
        # restore drill that checks them.
        backup = import ./tests/backup.nix { inherit self pkgs; };
        # The shared compile cache: one daemon per user, the compiles in the
        # builds that ask for them, and a plain compile whenever it can't help.
        rust-cache = import ./tests/rust-cache.nix { inherit self pkgs inputs; };
        # The ledger's pass over one repo, with a stand-in for cn.
        ledger = import ./tests/ledger.nix { inherit self pkgs; };
        # An App's tokens: who gets them, and git and gh acting as it.
        github = import ./tests/github.nix { inherit self pkgs; };
        # A switch that changes a handler's units kicks that handler's full
        # pass, and only its: the switch restarts an active unit that changed,
        # so the example's burrow-events-switched-autoland must change with
        # autoland's units (either of them) and stay active once started,
        # renovate's must not change, and its start must kick autoland, but
        # not at boot.
        events-switch = let
          base = self.nixosConfigurations.example;
          moved = svc: base.extendModules { modules = [{ systemd.services.${svc}.environment.MOVED = "1"; }]; };
          unit = c: h: let n = "burrow-events-switched-${h}.service"; in "${c.config.systemd.units.${n}.unit}/${n}";
          start = base.config.systemd.services.burrow-events-switched-autoland.serviceConfig.ExecStart;
        in pkgs.runCommand "events-switch-check" { } ''
          differ() { ! cmp -s "$1" "$2" || { echo "$3"; exit 1; }; }
          differ ${unit base "autoland"} ${unit (moved "autoland") "autoland"} "autoland.service changed, autoland's didn't"
          differ ${unit base "autoland"} ${unit (moved "autoland@") "autoland"} "autoland@.service changed, autoland's didn't"
          cmp -s ${unit base "renovate"} ${unit (moved "autoland") "renovate"} || { echo "autoland changed renovate's"; exit 1; }
          grep -qx RemainAfterExit=true ${unit base "autoland"} || { echo "not active once started"; exit 1; }
          grep -qx WantedBy=multi-user.target ${unit base "autoland"} || { echo "not started"; exit 1; }

          # Its start, with stand-ins for systemctl and curl.
          mkdir bin
          printf '#!${pkgs.runtimeShell}\necho "$STATE"\n' >bin/systemctl
          printf '#!${pkgs.runtimeShell}\necho "$*" >>%s/kicks\n' "$PWD" >bin/curl
          chmod +x bin/*
          PATH=$PWD/bin:$PATH STATE=starting ${start}
          [ ! -e kicks ] || { echo "kicked at boot: $(cat kicks)"; exit 1; }
          PATH=$PWD/bin:$PATH STATE=running ${start}
          grep -q ' http://127.0.0.1:[0-9]*/kick/autoland$' kicks || { echo "kicked: $(cat kicks)"; exit 1; }
          touch $out
        '';
        # The machine's git clones with a token-only URL, as Renovate does.
        git-token-url = pkgs.runCommand "git-token-url-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
          python3 -I ${./tests/git-token-url.py} ${self.nixosConfigurations.example.config.programs.git.package}/bin/git "$TMPDIR"
          touch $out
        '';
        # Renovate's setup (tests/renovate-checks.nix).
      } // import ./tests/renovate-checks.nix { inherit self pkgs; } // {
        # The example's house-sync reads the config its module writes (a
        # field the binary doesn't know fails it). The sandbox has no
        # /etc/burrow/house, so a pass is one for other blocks: it says so and
        # does nothing, before any gh call or lock.
        house-sync-config = let
          sync = pkgs.lib.getExe (builtins.head (builtins.filter (p: (p.name or "") == "house-sync")
            self.nixosConfigurations.example.config.environment.systemPackages));
        in pkgs.runCommand "house-sync-config-check" { } ''
          got=$(${sync} status)
          [ "$got" = "house-sync: the system has other house blocks than this pass; the pass for those does the work" ] ||
            { echo "got: $got"; exit 1; }
          touch $out
        '';
        # advisory-watch as the example machine runs it: the config built into
        # it reads (a field the binary doesn't know fails it), and a report,
        # which only reads, has nothing yet. Its rules are
        # crates/advisory-watch's tests.
        advisory-watch-config = let
          watch = self.nixosConfigurations.example.config.systemd.services.advisory-watch.serviceConfig.ExecStart;
        in pkgs.runCommand "advisory-watch-config-check" { } ''
          got=$(${watch} report 2>err) || { echo "advisory-watch report failed (exit $?): $(cat err)"; exit 1; }
          [ "$got" = "nothing seen yet" ] || { echo "advisory-watch report said: $got"; exit 1; }
          touch $out
        '';
        # flake-gate: a repo's CI gate step, run here as CI runs it, and
        # burrow's own evaluating apart, so a Claude job's gate holds little.
        flake-gate = pkgs.runCommand "flake-gate-check" { nativeBuildInputs = [ (pkgs.callPackage ./tools/flake-gate/package.nix { }) pkgs.git pkgs.yq-go pkgs.shellcheck ]; } ''
          shellcheck -s bash ${./tests/flake-gate.sh}
          bash ${./tests/flake-gate.sh} ${./.}
          touch $out
        '';
        # The house action's callers, burrow's and the template's: each runs
        # it on every push and whenever the title or body is edited, with a
        # token that can post its statuses, and the action hands the check
        # the event's PR and head. The check itself is crates/burrow-house,
        # gated as crates-*.
        house-callers = pkgs.runCommand "house-callers-check" { nativeBuildInputs = [ pkgs.yq-go ]; } ''
          for caller in ${./.github/workflows/house.yml} ${./templates/project/.github/workflows/house.yml}; do
            yq -e '.on.pull_request.types | sort | join(",") == "edited,opened,reopened,synchronize"' "$caller"
            yq -e '.jobs.house.permissions | to_entries | map(.key + "=" + .value) | join(",") == "contents=read,pull-requests=read,statuses=write"' "$caller"
            yq -e '.jobs.house.steps[-1].uses | test("^(\./|FieldmouseWorks/burrow/)\.github/actions/house(@|$)")' "$caller"
          done
          action=${./.github/actions/house/action.yml}
          # Built where rustup finds the crates' pinned toolchain.
          yq -e '.runs.steps[0]."working-directory" == "''${{ github.action_path }}/../../../crates"' "$action"
          yq -e '.runs.steps[-1].env.NUMBER == "''${{ github.event.pull_request.number }}"' "$action"
          yq -e '.runs.steps[-1].env.HEAD_SHA == "''${{ github.event.pull_request.head.sha }}"' "$action"
          touch $out
        '';
        # Stale Remote Control servers catch up once idle, with a stand-in for Claude.
        remote-control = import ./tests/remote-control.nix { inherit self pkgs; };
        # A session past its server's memory cap dies alone, with a stand-in for Claude.
        remote-control-memory = import ./tests/remote-control-memory.nix { inherit self pkgs; };
        # systemd-oomd leaves a Remote Control server whole, with a stand-in for Claude.
        remote-control-oomd = import ./tests/remote-control-oomd.nix { inherit self pkgs; };
        # The signed-in browser: Claude driving it, its logins kept, its ports
        # and profile kept from everyone else.
        browser = import ./tests/browser.nix { inherit self pkgs; };
        # lib/project.nix's secrets check fails a project with a token in any
        # tracked text file, and its log says where, never what.
        house-secrets-leak = import ./tests/house-secrets.nix { inherit self pkgs; };
        # lib/project.nix's package of a Rust workspace: every member's binaries.
        house-rust-package = let
          pkg = (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust = { }; }).packages.${system}.default;
        in pkgs.runCommand "house-rust-package-check" { } ''
          [ "$(${pkg}/bin/house-fixture-member)" = 42 ]
          touch $out
        '';
        # lib/project.nix's package of a workspace that links a native
        # library: the build found the one the project names.
        house-native-package = let
          pkg = (self.lib.project { self = ./tests/house/rust-native; systems = [ system ]; rust = houseNative; }).packages.${system}.default;
        in pkgs.runCommand "house-native-package-check" { } ''
          [ "$(${pkg}/bin/house-native)" = ${pkgs.zlib.version} ]
          touch $out
        '';
      }
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
      ) // {
        # lib/project.nix's `include`: the checks build from the repo's
        # root with the workspace and only the files named outside it (its
        # rust-toolchain.toml links to burrow's, outside it, so it dangles
        # there, where nothing reads it).
        house-include-source = pkgs.runCommand "house-include-source-check" { } ''
          cd ${houseInclude.checks.${system}.rust-test.src}
          got=$(find . ! -type d | sort)
          want=$(printf '%s\n' ./spec/fact.txt ./ws/Cargo.lock ./ws/Cargo.toml ./ws/rust-toolchain.toml ./ws/src/lib.rs)
          [ "$got" = "$want" ] || { printf 'the source has:\n%s\n' "$got"; exit 1; }
          touch $out
        '';
        # And a file outside them changes none of its Rust checks, with the
        # repo a store path, as a flake's self is: a path into it would make
        # them depend on all of it, and run again for any change at all.
        house-include-apart =
          let
            fixture = "tests/house/rust-include";
            # The repo as a store path, as a flake's self is, with only the
            # fixture and the toolchain file its own links to, less what
            # `keep` leaves out.
            copy = keep: (self.lib.project {
              self = builtins.path {
                path = ./.;
                name = "source";
                filter = path: _:
                  let rel = nixpkgs.lib.removePrefix "${toString ./.}/" (toString path); in
                  keep rel && nixpkgs.lib.any (p: rel == p || nixpkgs.lib.hasPrefix "${p}/" rel || nixpkgs.lib.hasPrefix "${rel}/" p)
                    [ fixture "crates/rust-toolchain.toml" ];
              };
              systems = [ system ];
              rust = { dir = "${fixture}/ws"; include = [ "${fixture}/spec/fact.txt" ]; };
            }).checks.${system};
            all = copy (_: true);
            less = copy (rel: rel != "${fixture}/spec/other.txt");
            moved = builtins.filter (n: nixpkgs.lib.hasPrefix "rust-" n && all.${n}.drvPath != less.${n}.drvPath) (builtins.attrNames all);
          in
          pkgs.runCommand "house-include-apart-check" { } (
            if moved == [ ] then "touch $out"
            else "echo ${nixpkgs.lib.escapeShellArg "${fixture}/spec/other.txt, which nothing includes, changes ${toString moved}"}; exit 1"
          );
      };

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
