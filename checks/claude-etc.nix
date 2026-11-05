# Claude Code watches /etc/claude-code and follows what it finds: a
# symlink there has it walking the Nix store for as long as it runs.
{ self, pkgs, ... }:
let
  etc = self.nixosConfigurations.example.config.environment.etc;
  names = builtins.filter (pkgs.lib.hasPrefix "claude-code/") (builtins.attrNames etc);
  links = builtins.filter (name: etc.${name}.mode == "symlink") names;
in pkgs.runCommand "claude-etc-check" { } ''
  ${pkgs.lib.optionalString (names == [ ]) ''echo "no claude-code files in /etc"; exit 1''}
  ${pkgs.lib.optionalString (links != [ ]) ''echo "symlinks into the store: ${toString links}"; exit 1''}
  touch $out
''
