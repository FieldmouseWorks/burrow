# claude-trust, as the machine gets it, on a stand-in ~/.claude.json:
# both spellings of a folder, the rest kept, Claude Code's lock kept to.
{ self, pkgs, ... }:
pkgs.runCommand "claude-trust-check" { nativeBuildInputs = [ pkgs.shellcheck pkgs.jq ]; } ''
  shellcheck -s bash ${../tests/claude-trust.sh}
  bash ${../tests/claude-trust.sh} ${pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.claude-trust}
  touch $out
''
