# Codex jobs run wherever proj does, so the guard as Codex's managed hook,
# on its commands and its apply_patch, and the TTY pin come with proj: on
# the example machine, and on one with proj on and burrow.claude off. So
# does the guard for proj's Claude jobs: burrow.claude's managed settings
# run it where they're installed, and proj gives it to its jobs' Claude
# Code where they aren't (its config's claudeGuard), a hook that answers.
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
      claudeOn = machine.config.burrow.claude.enable or false;
      hook = cmd: builtins.toJSON { tool_name = "Bash"; tool_input.command = cmd; cwd = "/"; };
      requirements = etc."codex/requirements.toml".source
        or (throw "${name}: proj is on, but Codex's requirements aren't installed");
    in ''
      echo ${name}
      ${pkgs.remarshal}/bin/toml2json ${requirements} > req.json
      jq -e '.features.unified_exec_tty == false' req.json
      jq -e '[.hooks.PreToolUse[] | select(.matcher == "Bash|apply_patch") | .hooks[].command] == ["${pkgs.lib.getExe machine.config.burrow.tools.codex-guard}"]' req.json
      grep -aq -- '--codex-user' ${pkgs.lib.getExe machine.config.burrow.tools.codex-guard}
      proj_config=$(grep -o 'PROJ_CONFIG=[^ ]*' ${pkgs.lib.getExe machine.config.burrow.tools.proj} | cut -d= -f2)
      ${if claudeOn then ''
        jq -e '.claudeGuard == null' "$proj_config"
      '' else ''
        jq -e '.claudeGuard.timeout > 0' "$proj_config"
        guard=$(jq -r '.claudeGuard.command' "$proj_config")
        got=$("$guard" <<<${pkgs.lib.escapeShellArg (hook "efibootmgr -o 0001")})
        jq -e '.hookSpecificOutput.permissionDecision == "ask"' <<<"$got"
      ''}
    '') machines;
in
assert !proj.config.burrow.claude.enable;
pkgs.runCommand "codex-guard-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  export XDG_RUNTIME_DIR=$TMPDIR
  ${pkgs.lib.concatStrings checks}
  touch $out
''
