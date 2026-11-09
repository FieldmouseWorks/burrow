# Every shared R2 pattern has a forbidden policy in the pinned CLI. Removing
# one rule must fail this very check: the check tests itself on that mutant.
{ self, pkgs, system, ... }:
let
  policy = import ../modules/claude/codex-rules.nix {
    inherit (pkgs) lib;
    patterns = import ../modules/claude/r2.nix;
  };
  expected = pkgs.writeText "r2-commands.json" (builtins.toJSON {
    patterns = import ../modules/claude/r2.nix;
    commands = policy.rules;
  });
  unmapped = pkgs.writeText "r2-unmapped.json" (builtins.toJSON {
    patterns = import ../modules/claude/r2.nix;
    commands = builtins.tail policy.rules;
  });
  rules = pkgs.writeText "r2.rules" policy.text;
  missing = pkgs.writeText "missing-rule.rules" (policy.render (builtins.tail policy.rules));
  agents = import ../modules/autoland/agents/package.nix { inherit pkgs; inherit (self.inputs) nixpkgs-unstable; };
  checker = "${self.packages.${system}.burrow-codex}/bin/burrow-codex-policy-check";
in
pkgs.runCommand "codex-r2" { } ''
  export CODEX_HOME=$TMPDIR/codex
  mkdir -p "$CODEX_HOME"
  ${checker} ${agents}/bin/codex ${expected} ${rules}
  if ${checker} ${agents}/bin/codex ${expected} ${missing} >mutant.log 2>&1; then
    echo "Removing an R2 rule passed the check" >&2
    exit 1
  fi
  if ${checker} ${agents}/bin/codex ${unmapped} ${rules} >unmapped.log 2>&1; then
    echo "An R2 pattern with no generated rule passed the check" >&2
    exit 1
  fi
  cat mutant.log unmapped.log
  touch $out
''
