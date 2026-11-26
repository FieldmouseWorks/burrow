# The machine's git clones with a token-only URL, as Renovate does, and a
# patched build doesn't re-run nixpkgs's upstream installCheckPhase (which
# skips every httpd test in the sandbox and flakes under parallel prove).
{ self, pkgs, ... }:
let
  git = self.nixosConfigurations.example.config.programs.git.package;
  stockGit = self.nixosConfigurations.example.pkgs.git;
in
assert pkgs.lib.assertMsg
  (git.drvPath != stockGit.drvPath -> !(git.doInstallCheck or false))
  "programs.git.package: a patched git must override doInstallCheck = false";
pkgs.runCommand "git-token-url-check" { nativeBuildInputs = [ pkgs.python3 ]; } ''
  python3 -I ${../tests/git-token-url.py} ${git}/bin/git "$TMPDIR"
  touch $out
''
