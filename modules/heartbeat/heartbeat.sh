# HOST-heartbeat: one beat. Tells the watcher off this machine how it is
# (../heartbeat.nix runs it every few minutes and sets HEARTBEAT, URL, CHECK,
# FAIL_AFTER, SKIP and HEALTH; the ping key is the unit's credential).
#
#   every health check passes (bar SKIP)   a success ping
#   one fails, fewer than FAIL_AFTER beats a log ping: on the record, no alert
#   one fails FAIL_AFTER beats running     a fail ping naming it: alerts at once
#
# Silence alerts too, once the check's period and grace run out: the machine
# down, hung or cut off. Failed units ride along in the body for whoever
# looks, but never fail a beat: a job's unit failing isn't the machine failing.

key=$(tr -d '[:space:]' <"${CREDENTIALS_DIRECTORY:?run as the $HEARTBEAT unit}/ping-key")
# How many beats running each check has failed. Lost on a reboot, which is fine.
streaks=${RUNTIME_DIRECTORY:?run as the $HEARTBEAT unit}

pass=0 down=() blips=() units=()
# HOST-health prints `check:NAME ok|fail` per check and `unit:NAME fail` per failed unit.
while read -r what state; do
  case $what in
    check:*)
      name=${what#check:}
      [[ " $SKIP " == *" $name "* ]] && continue
      if [ "$state" = ok ]; then
        pass=$((pass + 1))
        rm -f "$streaks/$name"
        continue
      fi
      n=$(($(cat "$streaks/$name" 2>/dev/null || echo 0) + 1))
      echo "$n" >"$streaks/$name"
      if [ "$n" -ge "$FAIL_AFTER" ]; then down+=("$name"); else blips+=("$name"); fi
      ;;
    unit:* | user-unit:*) units+=("${what#*:}") ;;
  esac
done < <("$HEALTH")

if [ ${#down[@]} -gt 0 ]; then
  suffix=/fail
  body="failing for $FAIL_AFTER beats or more: ${down[*]}"
  [ ${#blips[@]} -eq 0 ] || body+=$'\n'"failing for less: ${blips[*]}"
elif [ ${#blips[@]} -gt 0 ]; then
  suffix=/log
  body="failing, not for long yet: ${blips[*]}"
else
  suffix=
  body="healthy: $pass checks pass"
fi
[ ${#units[@]} -eq 0 ] || body+=$'\n'"failed units (not counted): ${units[*]}"

echo "${suffix:-/} $body"
# The key goes in through a config on a pipe, not argv, where any user on the
# machine could read it. A ping that doesn't land never fails the unit: the
# watcher already alerts when pings stop, and a failed unit would make
# HOST-update's health compare revert whatever it was switching to.
printf '%s\n' "$body" |
  curl -fsS --max-time 15 --retry 3 -o /dev/null --data-binary @- \
    -K <(printf 'url = "%s/%s/%s%s"\n' "$URL" "$key" "$CHECK" "$suffix") ||
  echo "the ping to $CHECK didn't land; Healthchecks alerts once the check's grace runs out" >&2
