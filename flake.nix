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
      names = [ "claude" "undo" "switch" "notify" "updates" "proj" "remote-control" "agents" "robot" "ops" "toolbox" "autoland" "house" "events" "ledger" "browser" "heartbeat" "backup" "watchdog" "github" "rust-cache" "cpu" "usage-reset" ];
      # gh-token (modules/github): its package, and its gate.
      # Built for every platform lib/project.nix knows, like the module.
      ghToken = self.lib.project {
        self = ./modules/github;
        rust.testInputs = pkgs: [ pkgs.openssl ];
      };
      # burrow's own programs, one Cargo workspace (crates/): their package
      # and their gate, built for every platform lib/project.nix knows.
      crates = self.lib.project { self = ./crates; rust = { }; };
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
      # platform they're built for, and the flaky tool (docs/flaky-tests.md),
      # for a machine to install.
      packages = nixpkgs.lib.foldl' nixpkgs.lib.recursiveUpdate { } [
        (nixpkgs.lib.mapAttrs (_: p: { burrow = p.default; }) crates.packages)
        (nixpkgs.lib.mapAttrs (_: p: { gh-token = p.default; }) ghToken.packages)
        { ${system}.flaky = pkgs.callPackage ./tools/flaky/package.nix { }; }
      ];

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
        in pkgs.runCommand "cpu-check" { } ''
          [ ${toString weight} = 400 ] || { echo "ci.slice weight ${toString weight}"; exit 1; }
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

          # A bounded wait ends on time, and never starts the command after.
          rm -f ran
          rm -f held; $h -- sh -c 'touch held; sleep 1.5' & ready $! test -e held
          s=$(date +%s%N); rc=0; $h --wait 1 -- touch ran 2>err || rc=$?; e=$(date +%s%N)
          [ "$rc" = 75 ] || { echo "--wait 1 with the slot freed after it: $rc, not 75"; exit 1; }
          [ ! -e ran ] || { echo "--wait 1 ran the command after its wait"; exit 1; }
          [ $(( (e - s) / 1000000 )) -lt 1800 ] || { echo "--wait 1 took $(( (e - s) / 1000000 ))ms"; exit 1; }
          rc=0; $h --wait 0 -- touch ran 2>err || rc=$?
          [ "$rc" = 75 ] && [ ! -e ran ] || { echo "--wait 0: $rc"; exit 1; }
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
        # The stall watchdog's ladder (kick, Claude job, stuck issue, owner),
        # with the real notify and stand-ins for proj, notify's gh and
        # burrow-events' kick port.
        watchdog = pkgs.runCommand "watchdog-check" { nativeBuildInputs = [ pkgs.python3 pkgs.bash ]; } ''
          python3 -I ${./tests/watchdog.py} ${./modules/watchdog}/watchdog.py ${./modules/notify}/notify.sh
          touch $out
        '';
        # The update job's fix jobs: pipeline work, one per problem while it's
        # recent.
        update-dispatch = pkgs.runCommand "update-dispatch-check" { nativeBuildInputs = [ pkgs.shellcheck ]; } ''
          shellcheck -s bash ${./modules/updates/dispatch.sh} ${./tests/update-dispatch.sh}
          bash ${./tests/update-dispatch.sh} ${./modules/updates/dispatch.sh}
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
        # Stale Remote Control servers catch up once idle, with a stand-in for Claude.
        remote-control = import ./tests/remote-control.nix { inherit self pkgs; };
        # A session past its server's memory cap dies alone, with a stand-in for Claude.
        remote-control-memory = import ./tests/remote-control-memory.nix { inherit self pkgs; };
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
      } // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "crates-${name}") crates.checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "gh-token-${name}") ghToken.checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "flaky-${name}")
        # The flaky tool's fmt, clippy and tests.
        (self.lib.project { inherit self; systems = [ system ]; rust.dir = "tools/flaky"; }).checks.${system}
      // nixpkgs.lib.mapAttrs' (name: nixpkgs.lib.nameValuePair "house-${name}") (
        # lib/project.nix's own gates, on the smallest projects that have them.
        (self.lib.project { self = ./tests/house/rust; systems = [ system ]; rust.testInputs = pkgs: [ pkgs.git ]; }).checks.${system}
        // (self.lib.project { self = ./tests/house/python; systems = [ system ]; python = { format = true; }; }).checks.${system}
      );

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
