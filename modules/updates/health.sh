# HOST-health: is the machine at least as healthy as it was a moment ago?
# (HEALTH and BURROW_USER come from ../updates.nix.)
#
#   HOST-health snapshot FILE              record which checks pass, and which units have failed
#   HOST-health compare FILE [SECONDS]     after a change: wait up to SECONDS (default 180) for
#                                          everything that passed in FILE to pass again, and for
#                                          no unit to have newly failed; exit 1 and say what
#                                          regressed if it doesn't
#   HOST-health                            run the checks now and print them
#
# The checks live in /etc/burrow/health.d (one small script each, from
# burrow.updates.health in the flake), so every module can add its own. Comparing
# against a snapshot means something that was already broken never blocks an
# update, and something an update breaks always does.

dir=/etc/burrow/health.d

run_checks() {
  local c
  for c in "$dir"/*; do
    [ -x "$c" ] || continue
    if timeout 30 "$c" >/dev/null 2>&1; then
      echo "check:$(basename "$c") ok"
    else
      echo "check:$(basename "$c") fail"
    fi
  done
  { systemctl list-units --failed --plain --no-legend --all || true; } | awk '{print "unit:" $1 " fail"}'
  { systemctl --user -M "$BURROW_USER@" list-units --failed --plain --no-legend --all 2>/dev/null || true; } |
    awk '{print "user-unit:" $1 " fail"}'
}

# Lines of what passed before (or was healthy before) but doesn't now.
regressions() {
  local before=$1 now=$2
  # A check that passed before and fails now. (Each grep may find nothing.)
  { grep '^check:.* ok$' "$before" || true; } | sed 's/ ok$//' | while read -r c; do
    if grep -qx "$c fail" "$now"; then echo "$c is failing"; fi
  done
  # A check that's new with this system and fails.
  { grep '^check:.* fail$' "$now" || true; } | sed 's/ fail$//' | while read -r c; do
    if ! grep -q "^$c " "$before"; then echo "$c (new) is failing"; fi
  done
  # A unit that failed since.
  { grep '^\(user-\)\?unit:' "$now" || true; } | while read -r u _; do
    if ! grep -q "^$u " "$before"; then echo "${u#*:} failed"; fi
  done
  return 0
}

case "${1:-}" in
  snapshot)
    run_checks >"${2:?usage: $HEALTH snapshot FILE}"
    ;;
  compare)
    before=${2:?usage: $HEALTH compare FILE [SECONDS]}
    deadline=$(($(date +%s) + ${3:-180}))
    now=$(mktemp)
    trap 'rm -f "$now"' EXIT
    while :; do
      # Let restarts and reconnects settle before judging.
      sleep 20
      run_checks >"$now"
      bad=$(regressions "$before" "$now")
      [ -n "$bad" ] || { echo "healthy"; exit 0; }
      [ "$(date +%s)" -lt "$deadline" ] || break
    done
    echo "$bad"
    exit 1
    ;;
  "")
    run_checks
    ;;
  *)
    echo "usage: $HEALTH [snapshot FILE | compare FILE [SECONDS]]" >&2
    exit 2
    ;;
esac
