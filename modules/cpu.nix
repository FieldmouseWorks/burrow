# cpu: how the machine shares its CPU between repos and runners. Gates (the
# slices that hold CI runners) get a larger CPUWeight than the Claudes and
# services around them, so a gate keeps its share whatever else runs; and
# `heavy` (crates/burrow-heavy) is one machine-wide set of slots for heavy work (a
# gate, a VM test, a replay, a large build), each with its share of the
# threads for cargo and test runners to size themselves by. A repo's own
# queue (the steward's proof slots, a repo's gate lock) decides which of its
# jobs goes next; heavy decides whether the machine has room for it now.
inputs:
{ config, lib, pkgs, ... }:
let
  cfg = config.burrow.cpu;
  dir = "/run/burrow-heavy";

  # The binary with its config; it reads every field, and refuses one it
  # doesn't know. argv0 stays `heavy`, as callers and `ps` name it.
  heavyConfig = pkgs.writeText "heavy.json" (builtins.toJSON {
    inherit (cfg.heavy) slots grace dir;
    setsid = "${pkgs.util-linux}/bin/setsid";
  });
  heavy = pkgs.runCommand "heavy" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "heavy";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow}/bin/burrow-heavy \
      $out/bin/heavy --inherit-argv0 --add-flags ${heavyConfig}
  '';
in
{
  imports = [ (import ./common.nix inputs) ];

  options.burrow.cpu = {
    enable = lib.mkEnableOption "CPU sharing: gates' CPU weight and `heavy`, the machine-wide heavy-work slots";

    gates = {
      slices = lib.mkOption {
        # A dash nests a slice (nix-builds.slice is in nix.slice), and a
        # weight only counts against siblings: a nested gate slice would
        # still meet user.slice at its parent's 100.
        type = lib.types.listOf (lib.types.strMatching "[A-Za-z0-9_]+");
        default = [ ];
        example = [ "ci" ];
        description = ''
          The system slices (names without `.slice`) that hold gates: CI
          runners and anything else someone waits on to merge. Each gets
          `gates.weight`. They sit at the top of the tree, beside user.slice
          and system.slice, so a name has no dash.
        '';
      };
      weight = lib.mkOption {
        type = lib.types.ints.between 1 10000;
        default = 400;
        description = ''
          The gate slices' CPUWeight, against systemd's 100 for user.slice
          (every Claude) and system.slice (nix-daemon's builds among them).
          At 400, gates busy together get two thirds of a machine that has
          no CPU to spare, and nothing at all when they're idle.
        '';
      };
    };

    heavy = {
      slots = lib.mkOption {
        type = lib.types.ints.positive;
        default = 2;
        description = ''
          How many heavy runs (`heavy -- COMMAND`) the machine takes at once,
          across every repo and account. Each gets the machine's threads
          divided by this in HEAVY_JOBS, CARGO_BUILD_JOBS and
          RUST_TEST_THREADS.
        '';
      };
      grace = lib.mkOption {
        type = lib.types.ints.positive;
        default = 10;
        internal = true;
        description = "Seconds a cancelled heavy run gets to stop before its processes are killed (the check's is shorter).";
      };
      dir = lib.mkOption {
        type = lib.types.str;
        default = dir;
        internal = true;
        description = "Where the slots' lock files are (the check's is a temporary one).";
      };
    };
  };

  config = lib.mkMerge [
    # Always built, so the check can build one of its own.
    { burrow.tools.heavy = heavy; }

    (lib.mkIf cfg.enable {
      environment.systemPackages = [ heavy ];
      systemd.slices = lib.genAttrs cfg.gates.slices (_: {
        sliceConfig.CPUWeight = cfg.gates.weight;
      });
      # Read-only to everyone, so runners under ProtectSystem=strict lock them too.
      systemd.tmpfiles.rules = [ "d ${cfg.heavy.dir} 0755 root root -" ]
        ++ map (i: "f ${cfg.heavy.dir}/slot${toString i} 0444 root root -") (lib.range 1 cfg.heavy.slots);

      burrow.claude.machineMd = lib.mkOrder 720 ''
        ## Heavy work: take a slot

        Wrap anything that keeps several cores busy for minutes (a gate, a
        full test suite, a VM test, a replay, a large build) in `heavy`:

            heavy -- COMMAND...               run it in one of ${toString cfg.heavy.slots} machine-wide slots
            heavy --wait 600 -- COMMAND...    give up (exit 75) after 10 minutes
            heavy status                      which slots are busy

        It waits for a free slot, then runs the command with HEAVY_JOBS (its
        share of the threads), CARGO_BUILD_JOBS and RUST_TEST_THREADS set:
        size your own parallelism (browser workers, `-j`) from HEAVY_JOBS.
        Calls inside a slot don't take another.

        Gates (${
          if cfg.gates.slices == [ ] then "none here yet" else lib.concatMapStringsSep ", " (s: "${s}.slice") cfg.gates.slices
        }) also outweigh every Claude for CPU, so a job's build yields to them.
        A repo's own queue still decides which of its jobs goes next.
      '';
    })
  ];
}
