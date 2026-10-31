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
      names = [ "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "toolbox" "autoland" "house" "events" "ledger" "browser" "heartbeat" "backup" "watchdog" "github" "rust-cache" "cpu" "dispatch" "usage-reset" ];
      # gh-token (modules/github): its package, and its gate.
      # Built for every platform lib/project.nix knows, like the module.
      ghToken = self.lib.project {
        self = ./modules/github;
        rust.testInputs = pkgs: [ pkgs.openssl ];
      };
      # burrow's own programs, one Cargo workspace (crates/): their package
      # and their gate, built for every platform lib/project.nix knows.
      crates = self.lib.project { self = ./crates; rust = { }; };
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

      # burrow's programs (every binary in crates/) and gh-token, for each
      # platform they're built for, the flaky tool (docs/flaky-tests.md) and
      # flake-gate, for a machine to install.
      packages = nixpkgs.lib.foldl' nixpkgs.lib.recursiveUpdate { } [
        (nixpkgs.lib.mapAttrs (_: p: { burrow = p.default; }) crates.packages)
        (nixpkgs.lib.mapAttrs (_: p: { gh-token = p.default; }) ghToken.packages)
        {
          ${system} = {
            flaky = pkgs.callPackage ./tools/flaky/package.nix { };
            flake-gate = pkgs.callPackage ./tools/flake-gate/package.nix { };
          };
        }
      ];

      # A bare machine with every module on, for the checks and as an example.
      nixosConfigurations.example = nixpkgs.lib.nixosSystem {
        modules = [ self.nixosModules.default ./examples/minimal.nix ];
      };

      checks.${system} = {
        example = self.nixosConfigurations.example.config.system.build.toplevel;
        # lib/project.nix's secrets check over all of burrow's tracked files;
        # the projects below each see only their own directory.
        secrets = (self.lib.project { inherit self; systems = [ system ]; }).checks.${system}.secrets;
        # Each module on its own, with only what it imports: evaluating is the
        # check, so these build nothing. A service that says how it runs (a
        # Type) has something to run too: systemd refuses one without
        # ExecStart or ExecStop, and only when it starts. A module that
        # registers a burrow-events handler has burrow-events to run it. And a
        # memory cap holds swap to it too: a unit that may swap outgrows its
        # cap without ever reaching it, and the machine thrashes rather than
        # the kernel killing inside the cap.
        standalone = let
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
        in pkgs.writeText "burrow-standalone" (nixpkgs.lib.concatMapStrings (name:
          let
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
                (nixpkgs.lib.optionalAttrs (name == "dispatch") { burrow.dispatch.runners.ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; }; })
                (nixpkgs.lib.optionalAttrs (name == "ledger") { burrow.ledger = { package = cnStandIn; owners = [ "sam" ]; anchorRepo = "sam/homelab"; reviewers = [ "sams-steward[bot]" ]; sessionLinks = [ "sam/homelab" ]; daemon = true; }; })
                (nixpkgs.lib.optionalAttrs (builtins.elem name [ "events" "autoland" ]) { burrow.events = { host = "example.org"; secretFile = "/run/secrets/burrow-webhook"; }; })
                # github has no switch: it's on with an App to serve.
                (if name == "github" then { burrow.github.apps.workers = { id = 2; slug = "sam-workers"; keyFile = "/run/secrets/workers-key"; }; }
                 else { burrow.${{ remote-control = "remoteControl"; rust-cache = "rustCache"; usage-reset = "usageReset"; }.${name} or name}.enable = true; })
              ];
            };
          in "${name} ${builtins.unsafeDiscardStringContext (runnable name machine).config.system.build.toplevel.drvPath}\n")
            names + "example ${builtins.unsafeDiscardStringContext (runnable "example" self.nixosConfigurations.example).config.system.build.toplevel.drvPath}\n");
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
        # proj's queue and memory caps, with a stand-in for Claude.
        proj = import ./tests/proj.nix { inherit self pkgs; };
        # A turn's clock, which stands still while it waits in line.
        proj-clock = import ./tests/proj-clock.nix { inherit self pkgs; };
        # Who takes a free slot: the pipeline's own (only its own repos'
        # pipeline jobs), and each project's cap.
        proj-pick = pkgs.runCommand "proj-pick-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.git ]; } ''
          shellcheck -s bash ${./modules/proj/pick.sh} ${./tests/proj-pick.sh}
          bash ${./tests/proj-pick.sh} ${./modules/proj/pick.sh}
          touch $out
        '';
        # What a look at the line costs, and who gets to skip one.
        proj-line = pkgs.runCommand "proj-line-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.git pkgs.strace pkgs.util-linux ]; } ''
          shellcheck -s bash ${./modules/proj/line.sh} ${./tests/proj-line.sh}
          bash ${./tests/proj-line.sh} ${./modules/proj/pick.sh} ${./modules/proj/line.sh}
          touch $out
        '';
        # What each job waits on, as the stall watchdog hears it.
        proj-waits = pkgs.runCommand "proj-waits-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.jq pkgs.util-linux ]; } ''
          shellcheck -s bash ${./modules/proj/waits.sh} ${./tests/proj-waits.sh}
          bash ${./tests/proj-waits.sh} ${./modules/proj/waits.sh}
          touch $out
        '';
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
        # The steward's tools reach a pass however it's started: by hand or
        # a Claude job (`autoland-steward kick OWNER/REPO`), with no openssl,
        # gh or proj of its own, gets them from the wrapper, ahead of its own
        # PATH but for a review's. Its gh is the machine's: in an App's env
        # (a job's), `status` and a project's clone go through the socket,
        # and a pass's own calls with the steward's token as given.
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
            for t in openssl gh git tar proj; do
              env -i PATH="$p" ${pkgs.bash}/bin/bash -c "command -v $t" >/dev/null || { echo "$sub: no $t on the steward's PATH"; exit 1; }
            done
          done
          p=$(env -i PATH=/caller ${pkgs.bash}/bin/bash path-of review)
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
              ./examples/machine.nix
              {
                burrow.dispatch = {
                  enable = true;
                  runners = {
                    ci-runner = { repo = "sam/homelab"; labels = [ "homelab" ]; };
                    bare = { repo = "sam/homelab"; labels = [ "bare" ]; defaultLabels = false; };
                  };
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
          dirty = machine { dirtyRev = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dirty"; } [ ];
          arm = machine { } [ { nixpkgs.hostPlatform = lib.mkForce "aarch64-linux"; } ];
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
        in
          assert lib.assertMsg (deploy == want) "dispatch: ${builtins.toJSON deploy}, not ${builtins.toJSON want}";
          assert lib.assertMsg (settings dirty == settings committed)
            "dispatch: a build from a dirty tree isn't the same tree committed: ${builtins.toJSON (settings dirty)}, ${builtins.toJSON (settings committed)}";
          # What a pass compares the own repo at: the switch stamps it.
          assert lib.assertMsg (revisions == [ "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-dirty" ])
            "dispatch: the systems are stamped ${builtins.toJSON revisions}";
          assert lib.assertMsg (labels committed == wantLabels "X64") "dispatch: x86_64's labels: ${builtins.toJSON (labels committed)}";
          assert lib.assertMsg (labels arm == wantLabels "ARM64") "dispatch: aarch64's labels: ${builtins.toJSON (labels arm)}";
          pkgs.runCommand "dispatch-config-check" { } ''
            ${self.nixosConfigurations.example.config.systemd.services.burrow-dispatch.serviceConfig.ExecStart} status
            ${committed.config.systemd.services.burrow-dispatch.serviceConfig.ExecStart} status
            touch $out
          '';
        # HOST-notify's issues, with a stand-in for gh: one a notice, closed
        # when cleared, and pending until GitHub has shown it closed.
        notify = pkgs.runCommand "notify-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.jq pkgs.bash ]; } ''
          shellcheck -s bash ${./tests/notify.sh}
          bash ${./tests/notify.sh} ${./modules/notify/notify.sh}
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
        # HOST-update's own PR in a train repo (modules/updates/propose.sh):
        # real git, with a stand-in for gh.
        update-propose = pkgs.runCommand "update-propose-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.git pkgs.jq ]; } ''
          shellcheck -s bash ${./modules/updates/propose.sh} ${./tests/update-propose.sh}
          bash ${./tests/update-propose.sh} ${./modules/updates/propose.sh}
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
          machine = origin: (self.nixosConfigurations.example.extendModules {
            modules = [{ burrow.updates.origin = lib.mkForce origin; }];
          }).config;
          train = origin: (machine origin).burrow.updates.throughTrain;
          refused = origin: lib.any (a: !a.assertion && lib.hasInfix "OWNER/NAME" a.message) (machine origin).assertions;
          forms = [
            "https://github.com/sam/homelab" "https://github.com/sam/homelab.git" "https://github.com/sam/homelab/"
            "https://github.com/Sam/HomeLab" "https://GitHub.com/sam/homelab.git" "https://github.com./sam/homelab"
            "HTTPS://github.com:443/sam/homelab" "git@github.com:sam/homelab.git" "git@github.com:sam/homelab"
            "git@GITHUB.COM.:sam/homelab/" "ssh://git@github.com/sam/homelab.git" "ssh://git@github.com:22/sam/homelab/"
            "ssh://git@GitHub.com.:22/sam/homelab"
          ];
          wrong = lib.filter (o: !train o || refused o) forms
            ++ lib.filter (o: train o || refused o) [
              "https://git.example.org/sam/homelab" "https://github.com/sam/other" "/srv/mirrors/github.com/sam/homelab"
              "https://git.example.org/github.com/sam/homelab"
            ]
            ++ lib.filter (o: !refused o) [
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
        # burrow-events for real: deliveries, kicks and wake-ups starting a
        # handler's units through polkit.
        events-service = import ./tests/events.nix { inherit self pkgs; };
        # HOST-switch's revert when the window runs out mid-activation.
        switch = import ./tests/switch.nix { inherit self pkgs; };
        # systemd-oomd kills what eats the memory before the machine drowns.
        oomd = import ./tests/oomd.nix { inherit self pkgs; };
        # systemd pets the board's own watchdog, whichever came up first.
        hardware-watchdog = import ./tests/hardware-watchdog.nix { inherit self pkgs; };
        # HOST-health's ZFS check: the pools the machine needs, by name or
        # GUID, with a stand-in zpool.
        health-zfs = import ./tests/health-zfs.nix { inherit self pkgs; };
        # heartbeat's beats, with stand-ins for HOST-health and Healthchecks.
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
          python3 -I ${./tests/renovate-sweep.py} ${./modules/updates/renovate-sweep.sh} ${./modules/updates/major-seal.py} ${./modules/autoland/seal.py} ${./modules/autoland/lock_wait.py}
          touch $out
        '';
        # Which repos Renovate's runs cover, with whose token: gh's login for
        # all of them, or a run each with its owner's App token.
        renovate-run = pkgs.runCommand "renovate-run-check" { nativeBuildInputs = [ pkgs.python3 pkgs.util-linux ]; } ''
          python3 -I ${./tests/renovate-run.py} ${./modules/updates/renovate.sh}
          touch $out
        '';
        # A lock file refresh's too-new releases go back or fail it, with
        # stand-ins for cargo, npm and the registries.
        lock-age = pkgs.runCommand "lock-age-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.json5 ])) pkgs.git ]; } ''
          export HOME=$TMPDIR
          python3 -I ${./tests/lock-age.py} ${./modules/updates/lock-age.py}
          touch $out
        '';
        # What decides a lock file refresh's release age in a Renovate config,
        # as Renovate itself migrates it, resolves its presets (at any scope)
        # and reads its durations; what it can't resolve or read fails.
        release-ages = pkgs.runCommand "release-ages-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
          export HOME=$TMPDIR
          read() { echo "$1" | ${pkgs.lib.getExe pkgs.nodejs_24} ${./modules/updates/release-ages.mjs} ${pkgs.renovate} 2>/dev/null; }
          ages() { read "$1" | jq -c .ages; }
          test "$(ages '{"extends": ["config:best-practices"]}')" = '[259200000]'
          test "$(ages '{"packageRules": [{"matchManagers": ["cargo"], "extends": ["security:minimumReleaseAgeNpm"]}]}')" = '[259200000]'
          test "$(ages '{"cargo": {"extends": ["security:minimumReleaseAgeNpm"]}}')" = '[259200000]'
          test "$(ages '{"cargo": {"minimumReleaseAge": "1 week 2 days"}, "minimumReleaseAge": null, "statusCheckNames": {"minimumReleaseAge": "x"}, "statusCheckWhen": {"minimumReleaseAge": "always"}}')" = '[777600000]'
          test "$(ages '{"extends": ["config:recommended", "helpers:pinGitHubActionDigests"]}')" = '[]'
          # Old settings Renovate migrates, at any scope.
          test "$(ages '{"stabilityDays": 14, "packageRules": [{"matchManagers": ["cargo"], "stabilityDays": 10}]}')" = '[1209600000,864000000]'
          # Where else policy comes from.
          test "$(read '{"configFileNames": ["ci/renovate config.json"]}' | jq -c '[.configFileNames, .inheritConfig]')" = '[["ci/renovate config.json"],false]'
          test "$(read '{}' | jq -c '[has("configFileNames"), .inheritConfig]')" = '[false,false]'
          test "$(read '{"configFileNames": null}' | jq -c '[has("configFileNames"), .configFileNames]')" = '[true,null]'
          test "$(read '{"force": {"inheritConfig": true}}' | jq -c .inheritConfig)" = true
          test "$(read '{"forkToken": "t"}' | jq -c .forkMode)" = true
          test "$(read '{"persistRepoData": true}' | jq -c .forkMode)" = false
          for bad in '{"extends": ["github>someone/presets"]}' '{"extends": ["config:nope"]}' '{"minimumReleaseAge": "soon"}' '{"minimumReleaseAge": 5}' \
                     '{"repositories": [{"repository": "x/y", "configFileNames": ["c.json"]}]}'; do
            if read "$bad"; then echo "took $bad"; exit 1; fi
          done
          touch $out
        '';
        # The example's Renovate config, as Renovate's own rules read it: a
        # pin on a GitHub repo of its trusted owner's (sam, any case) waits
        # for no soak; anyone else's still does, and so does a sam/... on
        # another host or a registry's package that claims a sam repo.
        renovate-soak = pkgs.runCommand "renovate-soak-check" { } ''
          export HOME=$TMPDIR
          got=$(${pkgs.lib.getExe pkgs.nodejs_24} ${./tests/renovate-soak.mjs} ${pkgs.renovate} \
            ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} \
            '{"datasource":"github-digest","packageName":"sam/homelab"}' \
            '{"datasource":"github-tags","packageName":"Sam/Tools"}' \
            '{"datasource":"git-refs","packageName":"https://github.com/sam/homelab.git","manager":"nix","sourceUrl":"https://github.com/sam/homelab"}' \
            '{"datasource":"github-digest","packageName":"actions/checkout"}' \
            '{"datasource":"github-tags","packageName":"samuel/x"}' \
            '{"datasource":"github-tags","packageName":"sam/tools","registryUrl":"https://ghe.example.com"}' \
            '{"datasource":"docker","packageName":"sam/tools","sourceUrl":"https://github.com/sam/tools"}' \
            '{"datasource":"npm","packageName":"homelab","manager":"npm","sourceUrl":"https://github.com/sam/homelab"}' \
            2>/dev/null)
          want='github-digest sam/homelab none
          github-tags Sam/Tools none
          git-refs https://github.com/sam/homelab.git none
          github-digest actions/checkout 3 days
          github-tags samuel/x 3 days
          github-tags sam/tools at https://ghe.example.com 3 days
          docker sam/tools 3 days
          npm homelab 3 days'
          [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
          touch $out
        '';
        # Which updates Renovate merges itself under the example's config, as
        # Renovate's own rules read it: all but a major, a commit pin of an
        # action (pinDigest) included. The steward leaves Renovate's own PRs
        # to Renovate, so one it won't merge sits green for good.
        renovate-automerge = pkgs.runCommand "renovate-automerge-check" { } ''
          export HOME=$TMPDIR
          got=$(${pkgs.lib.getExe pkgs.nodejs_24} ${./tests/renovate-automerge.mjs} ${pkgs.renovate} \
            ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
          want='major no
          minor yes
          patch yes
          pin yes
          pinDigest yes
          digest yes
          lockFileMaintenance yes'
          [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
          touch $out
        '';
        # The PR titles Renovate gives the example's updates, as Renovate's
        # own code writes them: each the house style's `area: plain
        # sentence`, "deps: ...", whichever type config:recommended would
        # give it. The title is the subject the steward squashes onto main.
        renovate-titles = pkgs.runCommand "renovate-titles-check" { } ''
          export HOME=$TMPDIR
          got=$(${pkgs.lib.getExe pkgs.nodejs_24} ${./tests/renovate-titles.mjs} ${pkgs.renovate} \
            ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
          want='deps: update dependency left-pad to 1.3.1
          deps: update dependency vitest to 3.2.0
          deps: update dependency vite to v7
          deps: lock file maintenance (npm)
          deps: update dependency net.fabricmc:fabric-loader to 0.20.1
          deps: lock file maintenance (gradle)
          deps: update rust crate tokio to 1.41.0
          deps: lock file maintenance (cargo)
          deps: update fieldmouseworks/burrow digest to 0fce5b8
          deps: lock file maintenance (nix)'
          [ "$got" = "$(sed 's/^ *//' <<<"$want")" ] || { echo "got: $got"; exit 1; }
          touch $out
        '';
        # What a dashboard's Repository Problems lists under the example's
        # config, by Renovate's own logger: not the refresh it was told to take
        # without a release timestamp, but still a warning that's a problem.
        renovate-problems = pkgs.runCommand "renovate-problems-check" { } ''
          export HOME=$TMPDIR
          got=$(LOG_LEVEL=fatal ${pkgs.lib.getExe pkgs.nodejs_24} ${./tests/renovate-problems.mjs} ${pkgs.renovate} \
            ${self.nixosConfigurations.example.config.systemd.services.renovate.environment.RENOVATE_CONFIG_FILE} 2>/dev/null)
          [ "$got" = artifactErrors ] || { echo "got: $got"; exit 1; }
          touch $out
        '';
        # Which repos house-sync passes over, and when it tells the owner, with
        # stand-ins for gh and the notifier.
        house-sync = pkgs.runCommand "house-sync-check" { nativeBuildInputs = [ pkgs.python3 pkgs.jq pkgs.util-linux pkgs.diffutils ]; } ''
          python3 -I ${./tests/house-sync.py} ${./modules/house/sync.sh} ${./house}
          touch $out
        '';
        # The shared CI's commit check (flake-check.yml's commits job), its
        # own script with a stand-in for gh: a PR commit signed by a key
        # GitHub can't match to its committer fails it.
        commit-signatures = pkgs.runCommand "commit-signatures-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) pkgs.jq ]; } ''
          python3 -I ${./tests/commit-signatures.py} ${./.github/workflows/flake-check.yml}
          touch $out
        '';
        # The shared CI's gate step with eval-apart (flake-check.yml), its own
        # script with nix evaluating test flakes for real and a stand-in for
        # its builds: one nix process per derivation, each a GC root until
        # it's built, every output checked as nix flake check checks it, and a
        # flake with an output it doesn't know refused.
        gate = pkgs.runCommand "gate-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) pkgs.git pkgs.jq pkgs.nix ]; } ''
          python3 -I ${./tests/gate.py} ${./.github/workflows/flake-check.yml}
          touch $out
        '';
        # flake-gate: a repo's CI gate step, run here as CI runs it, and
        # burrow's own evaluating apart, as a Claude job's memory needs.
        flake-gate = pkgs.runCommand "flake-gate-check" { nativeBuildInputs = [ (pkgs.callPackage ./tools/flake-gate/package.nix { }) pkgs.git pkgs.yq-go pkgs.shellcheck ]; } ''
          shellcheck -s bash ${./tests/flake-gate.sh}
          bash ${./tests/flake-gate.sh} ${./.}
          touch $out
        '';
        # The shared CI's attestation step (flake-check.yml), its own script
        # with stand-ins for curl and gh: a private repo's job signs with its
        # own token, its audience the digest of the statement kept beside it.
        attestation = pkgs.runCommand "attestation-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) pkgs.git pkgs.jq ]; } ''
          python3 -I ${./tests/attestation.py} ${./.github/workflows/flake-check.yml}
          touch $out
        '';
        # The shared CI's merge step (flake-check.yml), its own script on
        # real repos: a PR is checked merged with its base as the base is when
        # the job starts, and one that no longer merges cleanly fails.
        pr-base = pkgs.runCommand "pr-base-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) pkgs.git ]; } ''
          python3 -I ${./tests/pr-base.py} ${./.github/workflows/flake-check.yml}
          touch $out
        '';
        # The shared rust-first check (rust-first-check.yml), its own script
        # with a stand-in for gh: a PR that adds Python, or grows it past what
        # a fix adds, fails without a "Why not Rust:" line in its body, as a
        # commit status a newer run replaces; and its callers, burrow's and
        # the template's, run it again when the body is edited.
        rust-first = pkgs.runCommand "rust-first-check" { nativeBuildInputs = [ (pkgs.python3.withPackages (p: [ p.pyyaml ])) pkgs.jq ]; } ''
          python3 -I ${./tests/rust-first.py} ${./.github/workflows/rust-first-check.yml} \
            ${./.github/workflows/rust-first.yml} ${./templates/project/.github/workflows/rust-first.yml}
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
      } // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "crates-${name}") crates.checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "gh-token-${name}") ghToken.checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "flaky-${name}")
        # The flaky tool's fmt, clippy and tests.
        (self.lib.project { inherit self; systems = [ system ]; rust.dir = "tools/flaky"; }).checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "house-${name}") (
        # lib/project.nix's own gates, on the smallest projects that have them.
        (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust.testInputs = pkgs: [ pkgs.git ]; }).checks.${system}
        // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "native-${name}")
          (self.lib.project { self = ./tests/house/rust-native; systems = [ system ]; rust = houseNative; }).checks.${system}
        // (self.lib.project { self = ./tests/house/python; systems = [ system ]; python = { format = true; }; }).checks.${system}
      );

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
