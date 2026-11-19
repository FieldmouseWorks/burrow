# Claude Code's wrapper exports no LD_LIBRARY_PATH for the programs it runs to
# inherit: a QEMU built against the system's glibc died loading unstable's
# libasound through it (modules/claude/package.nix says why it's dropped).
{ self, pkgs, ... }:
let
  claude = self.nixosConfigurations.example.config.burrow.claude.package;
in pkgs.runCommand "claude-library-path-check" { } ''
  if grep -n LD_LIBRARY_PATH ${claude}/bin/claude; then
    echo "the claude wrapper sets LD_LIBRARY_PATH for everything it runs"; exit 1
  fi
  grep -q '^exec .*/\.claude-wrapped"' ${claude}/bin/claude || { echo "the claude wrapper no longer runs the binary"; exit 1; }
  touch $out
''
