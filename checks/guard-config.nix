# The example's guard, as Claude Code runs it, reads the config its
# module writes (a field the binary doesn't know makes it ask at
# every command): a plain command goes through, a one-way door asks.
{ self, pkgs, ... }:
let
  guard = pkgs.lib.getExe self.nixosConfigurations.example.config.burrow.tools.claude-guard;
  hook = cmd: builtins.toJSON { tool_name = "Bash"; tool_input.command = cmd; cwd = "/"; };
in pkgs.runCommand "guard-config-check" { } ''
  export XDG_RUNTIME_DIR=$TMPDIR
  got=$(${guard} <<<${pkgs.lib.escapeShellArg (hook "ls")})
  [ -z "$got" ] || { echo "ls: $got"; exit 1; }
  got=$(${guard} <<<${pkgs.lib.escapeShellArg (hook "efibootmgr -o 0001")})
  ${pkgs.jq}/bin/jq -e '.hookSpecificOutput.permissionDecision == "ask"' <<<"$got"
  touch $out
''
