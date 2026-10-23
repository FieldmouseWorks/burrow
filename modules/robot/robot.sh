# robot: a Hetzner dedicated server's out-of-band controls, through the Robot
# webservice. Works from the server or from any machine with the credentials
# (it's just curl), so it's also the way back in when the server itself is
# down. ../robot.nix sets ROBOT_DEFAULT_IP and ROBOT_DEFAULT_CREDS.
#
#   robot status                 server and reset/rescue state
#   robot rescue on [KEY_FP]     boot the Linux rescue system on next reset
#                                (KEY_FP: a Robot SSH key fingerprint; default:
#                                every key stored in Robot)
#   robot rescue off             cancel that
#   robot reset [sw|hw|power]    reset the box (default hw)
#   robot wol                    send wake-on-LAN
#
# Server: $ROBOT_SERVER_IP (default: this machine's). Credentials:
# $ROBOT_CREDS_FILE holding "user:password" for a Robot webservice user, or
# ROBOT_USER/ROBOT_PASSWORD.
# Cancelling the server and reinstalling it are deliberately not here; both
# are one-way doors and the guard asks before any raw call to them.

ip=${ROBOT_SERVER_IP:-$ROBOT_DEFAULT_IP}
url=${ROBOT_URL:-https://robot-ws.your-server.de}

creds() {
  if [ -n "${ROBOT_USER:-}" ]; then
    printf '%s:%s' "$ROBOT_USER" "${ROBOT_PASSWORD:-}"
  else
    f=${ROBOT_CREDS_FILE:-$ROBOT_DEFAULT_CREDS}
    if [ -r "$f" ]; then tr -d '\n' <"$f"
    elif [ -e "$f" ]; then sudo cat "$f" | tr -d '\n'
    else echo "no Robot credentials: put user:password in $f" >&2; exit 1
    fi
  fi
}

api() {
  local method=$1 path=$2; shift 2
  curl -fsS --max-time 30 -u "$(creds)" -X "$method" "$url$path" "$@"
}

case "${1:-status}" in
  status)
    api GET "/server/$ip" | jq '.server | {server_name, server_ip, product, dc, status}'
    api GET "/boot/$ip/rescue" | jq '.rescue | {active, os, authorized_key}'
    api GET "/reset/$ip" | jq '.reset | {type, operating_status}'
    ;;
  rescue)
    case "${2:-}" in
      on)
        args=(--data-urlencode os=linux)
        if [ -n "${3:-}" ]; then
          args+=(--data-urlencode "authorized_key[]=$3")
        else
          while read -r fp; do args+=(--data-urlencode "authorized_key[]=$fp"); done \
            < <(api GET /key | jq -r '.[].key.fingerprint')
        fi
        api POST "/boot/$ip/rescue" "${args[@]}" | jq '.rescue | {active, os, password}'
        echo "rescue armed: it boots on the next reset (robot reset)" >&2
        ;;
      off) api DELETE "/boot/$ip/rescue" | jq '.rescue | {active}' ;;
      *) echo "usage: robot rescue on [KEY_FP] | off" >&2; exit 2 ;;
    esac
    ;;
  reset)
    type=${2:-hw}
    case "$type" in sw|hw|power) ;; *) echo "reset type: sw, hw or power" >&2; exit 2 ;; esac
    api POST "/reset/$ip" --data-urlencode "type=$type" | jq '.reset'
    ;;
  wol) api POST "/wol/$ip" | jq '.wol' ;;
  *) echo "usage: robot status | rescue on [KEY_FP] | rescue off | reset [sw|hw|power] | wol" >&2; exit 2 ;;
esac
