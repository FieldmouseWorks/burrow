# The PreToolUse guard (crates/burrow-guard), with this machine's config (its
# src/config.rs): Claude Code's hook (claude.nix), and Codex's for proj's
# Codex jobs (proj.nix), which installs it whenever Codex jobs can run,
# burrow.claude on or off. A compiled wrapper rather than a script, since it
# runs on every tool call.
{ inputs, pkgs, lib, config }:
let
  # How long Claude Code and Codex give the hook; past it, the command runs
  # unchecked, so the guard answers before then.
  timeout = 10;

  guardConfig = pkgs.writeText "claude-guard.json" (builtins.toJSON {
    git = lib.getExe config.programs.git.package;
    zfs = "${config.boot.zfs.package}/bin/zfs";
    systemdRun = "${config.systemd.package}/bin/systemd-run";
    snapshotDatasets = lib.optionals (config.burrow.undo.enable or false) config.burrow.undo.datasets;
    askAfterSeconds = timeout - 2;
    # The label only the owner puts on: a proj job's command that would put
    # it on is asked about, which a job can't answer (the crate's first.rs).
    # None where the steward, which reads it, isn't on.
    firstLabel = if config.burrow.autoland.enable or false then config.burrow.autoland.labels.first else null;
  });
  guardWith = name: flags: pkgs.runCommand name {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = name;
  } ''
    makeBinaryWrapper ${inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-guard}/bin/burrow-guard \
      $out/bin/${name} --add-flags ${lib.escapeShellArg flags}
  '';
  # Codex's refuses where Claude's asks, since Codex runs a command its hook
  # asks about, and keeps a job's commits in reach of its git hooks (the
  # crate's codex.rs). Only the jobs' user's commands are guarded.
  codex = guardWith "codex-guard" "--codex-user ${config.burrow.user} ${guardConfig}";
in
{
  inherit timeout codex;
  claude = guardWith "claude-guard" "${guardConfig}";

  # Codex's requirements, which no job can turn off or skip.
  codexRequirements = (pkgs.formats.toml { }).generate "codex-requirements.toml" {
    # No TTY sessions: the hook sees the command that starts a process,
    # never what write_stdin types into it afterwards, so a shell kept open
    # would run anything past the guard. Without a TTY, a process's stdin
    # is closed. Every Codex here does without them, the reviewer too.
    features.unified_exec_tty = false;
    hooks.PreToolUse = [
      {
        # Commands, and Codex's own apply_patch, which deletes and replaces
        # files with no command: the undo snapshot comes before either.
        matcher = "Bash|apply_patch";
        hooks = [
          {
            type = "command";
            command = lib.getExe codex;
            inherit timeout;
          }
        ];
      }
    ];
  };
}
