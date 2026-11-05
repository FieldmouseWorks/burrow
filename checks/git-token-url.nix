# The machine's git clones with a token-only URL, as Renovate does.
{ self, pkgs, ... }:
pkgs.runCommand "git-token-url-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 -I ${../tests/git-token-url.py} ${self.nixosConfigurations.example.config.programs.git.package}/bin/git "$TMPDIR"
  touch $out
''
