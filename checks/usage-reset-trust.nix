# The usage watcher's Claude Code starts in a folder its unit has
# trusted already, so it never meets that question with nobody there:
# under a state dir with a space in it too, one argument however it's
# spelled (systemd's quoting, which bash reads the same for this one).
{ self, pkgs, ... }:
let
  example = (self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.stateDir = pkgs.lib.mkForce "/var/lib/burrow state"; }];
  }).config;
  pre = map (pkgs.lib.removePrefix "-")
    (pkgs.lib.toList (example.systemd.services.burrow-usage-reset.serviceConfig.ExecStartPre or [ ]));
in pkgs.runCommand "usage-reset-trust-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  export HOME=$TMPDIR/home
  mkdir -p "$HOME"
  echo '{}' >"$HOME/.claude.json"
  ${pkgs.lib.concatLines pre}
  # Where the watcher starts it: its state dir's claude/.
  dir=${pkgs.lib.escapeShellArg "${example.burrow.stateDir}/usage-reset/claude"}
  jq -e --arg d "$dir" '.projects[$d].hasTrustDialogAccepted == true' "$HOME/.claude.json" >/dev/null ||
    { echo "burrow-usage-reset starts its Claude Code in $dir untrusted" >&2; exit 1; }
  touch $out
''
