# Codex jobs run wherever proj does, so the guard as Codex's managed hook,
# on its commands and its apply_patch, and the TTY pin come with proj: on
# the example machine, and on one with proj on and burrow.claude off.
{ self, pkgs, nixpkgs, ... }:
let
  proj = nixpkgs.lib.nixosSystem {
    modules = [
      self.nixosModules.proj
      ../examples/machine.nix
      { burrow.proj.enable = true; }
    ];
  };
  machines = { example = self.nixosConfigurations.example; proj-alone = proj; };
  checks = nixpkgs.lib.mapAttrsToList (name: machine:
    let
      etc = machine.config.environment.etc;
      requirements = etc."codex/requirements.toml".source
        or (throw "${name}: proj is on, but Codex's requirements aren't installed");
    in ''
      echo ${name}
      ${pkgs.remarshal}/bin/toml2json ${requirements} > req.json
      jq -e '.features.unified_exec_tty == false' req.json
      jq -e '[.hooks.PreToolUse[] | select(.matcher == "Bash|apply_patch") | .hooks[].command] == ["${pkgs.lib.getExe machine.config.burrow.tools.codex-guard}"]' req.json
      grep -aq -- '--codex-user' ${pkgs.lib.getExe machine.config.burrow.tools.codex-guard}
    '') machines;
in
assert !proj.config.burrow.claude.enable;
pkgs.runCommand "codex-guard-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  ${pkgs.lib.concatStrings checks}
  touch $out
''
