# HOST-health's claude-login check (burrow-claude-login), run against a
# stand-in Claude Code: it passes only on a live reading of the usage meter,
# and fails on an error, on the null-limits and plan-less answers a failed
# usage endpoint gives with a `success` response, on silence and on a crash.
# The failure's message says what the owner does about it.
#
#   nix build .#checks.x86_64-linux.health-claude-login -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  config = self.nixosConfigurations.example.config;
  inherit (config.burrow) user;
  probe = "${self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-usage-reset}/bin/burrow-claude-login";
  # Claude Code, as far as the check can tell: it reads the request from
  # stdin and answers per $LOGIN. It records that it was asked get_usage.
  claude = pkgs.writeShellScript "claude" ''
    read -r request
    case "$request" in *'"subtype":"get_usage"'*) echo asked >>"$ASKED" ;; esac
    reply() { echo '{"type":"control_response","response":{"subtype":"success","request_id":"usage","response":'"$1"'}}'; }
    case "$LOGIN" in
      ok) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"},"five_hour":null,"limits":[]}}' ;;
      null-limits) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":null}}' ;;
      cached) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"}}}' ;;
      no-plan) reply '{"rate_limits_available":false,"subscription_type":null}' ;;
      expired) echo '{"type":"control_response","response":{"subtype":"error","request_id":"usage","error":"OAuth session expired and could not be refreshed"}}' ;;
      silent) ;;
      broken) exit 1 ;;
    esac
  '';
in
assert lib.hasInfix "burrow-claude-login" (builtins.unsafeDiscardStringContext config.burrow.updates.health.claude-login);
assert lib.hasInfix "Log in again on this machine as ${user}" config.burrow.updates.health.claude-login;
pkgs.runCommand "health-claude-login-check" { } ''
  export ASKED=$PWD/asked HOME=$PWD
  LOGIN=ok ${probe} ${claude} || { echo "a working login failed"; exit 1; }
  [ -s asked ] || { echo "Claude Code wasn't asked"; exit 1; }
  for login in null-limits cached no-plan expired silent broken; do
    ! LOGIN=$login ${probe} ${claude} 2>err || { echo "$login passed"; exit 1; }
  done

  # Persisted copies of the meter (fetchedAtMs, ms): the check never waits for
  # one to age, so each case is bounded by the health runner's 30s timeout.
  copy() { echo '{"cachedUsageUtilization":{"fetchedAtMs":'$(( $(date +%s%3N) + $1 ))'}}' >$HOME/.claude.json; }
  timed() { s=$SECONDS; "$@"; r=$?; [ $((SECONDS - s)) -lt 25 ] || { echo "took over the health timeout"; exit 1; }; return $r; }
  copy -10000; rm -f asked
  LOGIN=expired timed ${probe} ${claude} || { echo "a 10s-old copy failed"; exit 1; }
  [ ! -e asked ] || { echo "a 10s-old copy was asked past"; exit 1; }
  copy -3600000
  LOGIN=ok timed ${probe} ${claude} || { echo "an old copy failed a working login"; exit 1; }
  [ -s asked ] || { echo "an old copy wasn't asked past"; exit 1; }
  ! LOGIN=expired timed ${probe} ${claude} 2>err || { echo "an old copy passed an expired login"; exit 1; }
  copy 3600000
  ! LOGIN=ok timed ${probe} ${claude} 2>err || { echo "a future-dated copy passed"; exit 1; }
  copy -3600000
  rm -f $HOME/.claude.json
  ! LOGIN=silent timed ${probe} ${claude} 2>err || { echo "a stalled request passed"; exit 1; }
  touch $out
''
