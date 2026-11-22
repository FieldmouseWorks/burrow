# HOST-health's claude-login check (burrow-claude-login), run against a
# stand-in Claude Code: it passes only on a live reading of the usage meter, whatever a persisted copy says,
# and fails on an error, on the null-limits and plan-less answers a failed
# usage endpoint gives with a `success` response, on silence and on a crash.
# A cached answer on the endpoint's rate limit (a 429 in its debug log)
# passes; one on a 401, or with no plan limits, still fails.
# It is run as the example machine's own check text, so what the owner is
# told is tested: to log in again only when the login itself failed (no plan
# limits, or a refresh the token endpoint refused), else the probe's reason.
#
#   nix build .#checks.x86_64-linux.health-claude-login -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  config = self.nixosConfigurations.example.config;
  inherit (config.burrow) user;
  # Claude Code, as far as the check can tell: it reads the request from
  # stdin and answers per $LOGIN, writing its debug log where --debug-file
  # says as 2.1.330 does. It records that it was asked get_usage.
  claude = pkgs.writeShellScriptBin "claude" ''
    log=/dev/null
    while [ $# -gt 0 ]; do
      case "$1" in --debug-file) log=$2; shift ;; esac
      shift
    done
    refused() {
      echo "[DEBUG] fetchUtilization: GET /api/oauth/usage (attempt 1)" >>"$log"
      echo "[DEBUG] fetchUtilization: $1 remembered for this bearer; not asking again for 300s" >>"$log"
      echo "[ERROR] Failed to load usage data: Request failed with status code $1" >>"$log"
    }
    read -r request
    case "$request" in *'"subtype":"get_usage"'*) echo asked >>"$ASKED" ;; esac
    logged() { echo "[ERROR] $1" >>"$log"; }
    reply() { echo '{"type":"control_response","response":{"subtype":"success","request_id":"usage","response":'"$1"'}}'; }
    case "$LOGIN" in
      ok) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"},"five_hour":null,"limits":[]}}' ;;
      null-limits) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":null}}' ;;
      cached) reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"}}}' ;;
      no-plan) reply '{"rate_limits_available":false,"subscription_type":null}' ;;
      rate-limited) refused 429; reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"}}}' ;;
      rate-limited-no-plan) refused 429; reply '{"rate_limits_available":false,"subscription_type":null}' ;;
      refused) refused 401; reply '{"rate_limits_available":true,"rate_limits":{"seven_day":{"utilization":10.0,"resets_at":"2030-01-01T00:00:00Z"}}}' ;;
      expired)
        logged "OAuth refresh failed (expected): Request failed with status code 400"
        reply '{"rate_limits_available":false,"subscription_type":null,"rate_limits":null}' ;;
      revoked)
        logged "OAuth refresh failed (expected): Request failed with status code 400"
        reply '{"rate_limits_available":true,"rate_limits":null}'
        # 2.1.330 appends its log behind its answer.
        sleep 0.2; refused 401 ;;
      refresh-down)
        logged '"AxiosError: [url=https://platform.claude.com/v1/oauth/token,status=503] AxiosError: Request failed with status code 503"'
        refused 401; reply '{"rate_limits_available":true,"rate_limits":null}' ;;
      forbidden) refused 403; reply '{"rate_limits_available":true,"rate_limits":null}' ;;
      server-error)
        logged '"AxiosError: [url=/api/oauth/usage,status=500,body=not for the journal] AxiosError: Request failed with status code 500"'
        reply '{"rate_limits_available":true,"rate_limits":null}' ;;
      offline)
        logged "OAuth refresh failed (expected): connect ECONNREFUSED 127.0.0.1:9"
        reply '{"rate_limits_available":true,"rate_limits":null}'
        sleep 0.2; logged "Failed to load usage data: connect ECONNREFUSED 127.0.0.1:9" ;;
      errored) echo '{"type":"control_response","response":{"subtype":"error","request_id":"usage","error":"get_usage is not supported in this context"}}' ;;
      silent) ;;
      broken) exit 1 ;;
    esac
  '';
  machine = self.nixosConfigurations.example.extendModules {
    modules = [{ burrow.claude.package = lib.mkForce claude; }];
  };
  health = machine.config.burrow.updates.health.claude-login;
  # The check's own text, run as the user and as root where runuser fails.
  check = pkgs.writeShellScript "claude-login" ''
    id() { echo ${user}; }
    ${health}
  '';
  cantRun = pkgs.writeShellScript "claude-login-cant-run" ''
    id() { echo root; }
    runuser() { echo "runuser: cannot open session" >&2; return 1; }
    ${health}
  '';
in
assert lib.hasInfix "burrow-claude-login" (builtins.unsafeDiscardStringContext config.burrow.updates.health.claude-login);
pkgs.runCommand "health-claude-login-check" { } ''
  export ASKED=$PWD/asked HOME=$PWD
  LOGIN=ok ${check} || { echo "a working login failed"; exit 1; }
  [ -s asked ] || { echo "Claude Code wasn't asked"; exit 1; }
  LOGIN=rate-limited ${check} || { echo "the endpoint's rate limit failed a working login"; exit 1; }
  again="Log in again on this machine as ${user} (run claude, then /login)."
  login_failed() {
    grep -qF "$again" err || { echo "$1 wasn't told to log in again: $(cat err)"; exit 1; }
  }
  # Everything else passes on the probe's reason, WHY, and asks for no login.
  not_the_login() {
    ! grep -qiE "log in again|login expired" err || { echo "$1 was told to log in again: $(cat err)"; exit 1; }
    grep -qF "$2" err || { echo "$1 didn't say '$2': $(cat err)"; exit 1; }
  }
  for login in no-plan rate-limited-no-plan expired revoked; do
    ! LOGIN=$login ${check} 2>err || { echo "$login passed"; exit 1; }
    login_failed $login
  done
  while IFS=: read -r login why; do
    ! LOGIN=$login ${check} 2>err </dev/null || { echo "$login passed"; exit 1; }
    not_the_login $login "$why"
    ! grep -qF "not for the journal" err || { echo "$login copied a response body"; exit 1; }
  done <<'CASES'
  null-limits:no weekly utilization
  cached:usage endpoint not answering
  refused:status code 401
  refresh-down:status code 401
  forbidden:status code 403
  server-error:status code 500
  offline:connect ECONNREFUSED
  errored:not supported in this context
  silent:exited before it answered
  broken:burrow-claude-login:
  CASES
  ! ${cantRun} 2>err || { echo "a check that couldn't run passed"; exit 1; }
  not_the_login cantRun "cannot open session"

  # Persisted copies of the meter (fetchedAtMs, ms): a recent one proves
  # nothing about the login now, so it is waited out and a live reading asked;
  # every case here is over in 85s, well inside the 150s the health runner
  # gives this check (modules/updates.nix checkTimeouts.claude-login).
  copy() { echo '{"cachedUsageUtilization":{"fetchedAtMs":'$(( $(date +%s%3N) + $1 ))'}}' >$HOME/.claude.json; }
  timed() { s=$SECONDS; "$@"; r=$?; [ $((SECONDS - s)) -lt 85 ] || { echo "took over 85s"; exit 1; }; return $r; }
  copy -55000; rm -f asked
  ! LOGIN=expired timed ${check} 2>err || { echo "a recent copy passed an expired login"; exit 1; }
  login_failed "a recent copy's expired login"
  copy -55000; rm -f asked
  LOGIN=ok timed ${check} || { echo "a recent copy failed a working login"; exit 1; }
  [ -s asked ] || { echo "a recent copy wasn't asked past"; exit 1; }
  copy -3600000
  LOGIN=ok timed ${check} || { echo "an old copy failed a working login"; exit 1; }
  [ -s asked ] || { echo "an old copy wasn't asked past"; exit 1; }
  ! LOGIN=expired timed ${check} 2>err || { echo "an old copy passed an expired login"; exit 1; }
  copy 3600000
  ! LOGIN=ok timed ${check} 2>err || { echo "a future-dated copy passed"; exit 1; }
  not_the_login future-dated "snapshot"
  copy -3600000
  rm -f $HOME/.claude.json
  ! LOGIN=silent timed ${check} 2>err || { echo "a stalled request passed"; exit 1; }
  touch $out
''
