# lib/project.nix's guidance-paths check, on the drift that started it:
# remi's docs/recovery.md told readers to copy burrow's
# `modules/robot/robot.sh` after that script became crates/burrow-robot.
# That text fails a project's gate, with the file, line and target in the
# log; the corrected text passes; the same old text passes against a burrow
# pinned before the script went, so the pin decides; and a doc citing a
# script passes until the script is deleted, since the check reads the
# whole tree and not only its Markdown.
#
#   nix build .#checks.x86_64-linux.house-cited-paths -L
{ self, pkgs, nixpkgs, inputs, system, ... }:
let
  inherit (pkgs) lib;
  # A tree of FILE = TEXT.
  project = name: files: pkgs.runCommand name { } ''
    mkdir $out && cd $out
    ${lib.concatStrings (lib.mapAttrsToList (f: text: ''
      mkdir -p "$(dirname ${lib.escapeShellArg f})"
      printf '%s\n' ${lib.escapeShellArg text} > ${lib.escapeShellArg f}
    '') files)}
  '';
  # Its gate's guidance-paths check, with the burrow it pins.
  gate = burrow: src:
    (import ../lib/project.nix { inherit nixpkgs burrow; inherit (inputs) rust-overlay crane; } {
      self = src;
      systems = [ system ];
    }).checks.${system}.guidance-paths;
  remi = recovery: project "remi" {
    "modules/burrow.nix" = "{ }";
    "docs/recovery.md" = recovery;
  };
  before = remi ''
    anywhere with the Robot credentials (`robot` is a plain bash + curl script:
    copy burrow's `modules/robot/robot.sh`, set `ROBOT_SERVER_IP=46.4.33.93`,'';
  after = remi ''
    anywhere with the Robot credentials (`robot` is burrow's
    `crates/burrow-robot`: build it, set `ROBOT_SERVER_IP=46.4.33.93`,'';
  # burrow as it was before the script became a crate.
  oldBurrow = self // { outPath = project "old-burrow" { "modules/robot/robot.sh" = "#!/bin/sh"; }; };
  # tools/ stays: a path whose whole top-level directory is gone isn't
  # taken for the repository's (paths.rs).
  cites = project "cites" {
    "tools/run.sh" = "#!/bin/sh";
    "tools/other.sh" = "#!/bin/sh";
    "docs/run.md" = "Run `tools/run.sh`.";
  };
  scriptGone = project "script-gone" {
    "tools/other.sh" = "#!/bin/sh";
    "docs/run.md" = "Run `tools/run.sh`.";
  };
  retired = pkgs.testers.testBuildFailure (gate self before);
  deleted = pkgs.testers.testBuildFailure (gate self scriptGone);
in
pkgs.runCommand "house-cited-paths-check" { } ''
  expect() {
    if ! grep -qxF -- "$2" "$1/testBuildFailure.log"; then
      echo "expected '$2' in the log:"; cat "$1/testBuildFailure.log"; exit 1
    fi
  }
  expect ${retired} "docs/recovery.md:2: cites modules/robot/robot.sh, which the pinned burrow doesn't have"
  expect ${deleted} "docs/run.md:1: cites tools/run.sh, which this repository doesn't have"
  # These built: the corrected text, the old one against the old pin, and
  # the doc while its script is there.
  : ${gate self after} ${gate oldBurrow before} ${gate self cites}
  touch $out
''
