# The PreToolUse guard (crates/burrow-guard), with this machine's config (its
# src/config.rs): Claude Code's hook (../anthropic.nix). A compiled wrapper
# rather than a script, since it runs on every tool call.
{ inputs, pkgs, lib, config }:
let
  # How long Claude Code gives the hook; past it, the command runs
  # unchecked, so the guard answers before then.
  timeout = 10;

  guardConfig = pkgs.writeText "claude-guard.json" (builtins.toJSON {
    git = lib.getExe config.programs.git.package;
    zfs = "${config.boot.zfs.package}/bin/zfs";
    snapshotDatasets = lib.optionals (config.burrow.undo.enable or false) config.burrow.undo.datasets;
    askAfterSeconds = timeout - 2;
  });
in
{
  inherit timeout;
  claude = pkgs.runCommand "claude-guard" {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "claude-guard";
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-guard}/bin/burrow-guard \
      $out/bin/claude-guard --add-flags ${lib.escapeShellArg "${guardConfig}"}
  '';
}
