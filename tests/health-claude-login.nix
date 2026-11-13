# HOST-health's claude-login check, run against a stand-in Claude Code: it
# asks Claude Code itself (get_usage, no model call) and fails when the answer
# isn't a success, whether Claude Code says the login expired, answers
# nothing, or can't run. The failure says what the owner does about it.
#
#   nix build .#checks.x86_64-linux.health-claude-login -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  config = self.nixosConfigurations.example.config;
  inherit (config.burrow) user;
  # Claude Code, as far as the check can tell: it reads the request from
  # stdin and answers per $LOGIN. It records that it was asked get_usage.
  claude = pkgs.writeShellScriptBin "claude" ''
    read -r request
    case "$request" in *'"subtype":"get_usage"'*) echo asked >>"$ASKED" ;; esac
    case "$LOGIN" in
      ok) echo '{"type":"control_response","response":{"subtype":"success","request_id":"usage","response":{}}}' ;;
      expired) echo '{"type":"control_response","response":{"subtype":"error","request_id":"usage","error":"OAuth session expired and could not be refreshed"}}' ;;
      silent) ;;
      broken) exit 1 ;;
    esac
  '';
  # The check runs as the job user: say so, as the build sandbox's user isn't.
  id = pkgs.writeShellScriptBin "id" "echo ${user}";
  check = pkgs.writeShellScript "health-claude-login" ''
    export PATH=${lib.makeBinPath [ claude id pkgs.coreutils pkgs.gnugrep pkgs.jq ]}
    ${config.burrow.updates.health.claude-login}
  '';
in
assert config.burrow.updates.health ? claude-login;
pkgs.runCommand "health-claude-login-check" { } ''
  export ASKED=$PWD/asked
  LOGIN=ok ${check} || { echo "a working login failed"; exit 1; }
  [ -s asked ] || { echo "Claude Code wasn't asked"; exit 1; }
  for login in expired silent broken; do
    ! LOGIN=$login ${check} 2>err || { echo "$login passed"; exit 1; }
    grep -qF "Log in again on this machine as ${user}" err || { echo "$login: no instruction"; cat err; exit 1; }
  done
  touch $out
''
