# heavy [--wait SECONDS] -- COMMAND...  |  heavy status
#
# Runs COMMAND in one of the machine's SLOTS heavy-work slots, shared by every
# repo, runner and account: a gate, a VM test, a replay, a large build. When
# all are busy it waits for one (up to --wait seconds, then exits 75). The
# command gets the slot's share of the CPU in HEAVY_JOBS, and CARGO_BUILD_JOBS
# and RUST_TEST_THREADS from it unless they're set already. It runs with the
# slot's descriptor closed, so a daemon it leaves behind can't hold the slot,
# and its exit status passes through. Inside a slot (HEAVY_SLOT set) it just
# runs the command, so wrapped tools can call each other.
#
# A slot is a lock file in DIR that anyone may read; flock(2) needs no more,
# so runners with a read-only /run share them too.

usage() { echo "usage: heavy [--wait SECONDS] -- COMMAND...  |  heavy status" >&2; exit 2; }

threads=$(nproc)
share=$(( threads / SLOTS ))
[ "$share" -ge 1 ] || share=1

if [ "${1:-}" = status ]; then
  echo "$SLOTS slots of $share threads (of $threads)"
  # Trying each lock is the one test that works in every PID namespace:
  # /proc/locks names flock(1)'s process, long gone, as the holder.
  for i in $(seq 1 "$SLOTS"); do
    exec {fd}<"$DIR/slot$i" || { echo "slot$i: missing"; continue; }
    if flock -n "$fd"; then echo "slot$i: free"; else echo "slot$i: busy"; fi
    exec {fd}<&-
  done
  exit 0
fi

wait_for=
while [ $# -gt 0 ]; do
  case "$1" in
    --wait) [ $# -ge 2 ] || usage; wait_for=$2; shift 2 ;;
    --) shift; break ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || usage
case "$wait_for" in ''|*[!0-9]*) [ -z "$wait_for" ] || usage ;; esac

if [ -n "${HEAVY_SLOT:-}" ]; then exec "$@"; fi

deadline=
[ -z "$wait_for" ] || deadline=$(( $(date +%s) + wait_for ))
said=
while true; do
  for i in $(seq 1 "$SLOTS"); do
    exec {fd}<"$DIR/slot$i" || { echo "heavy: cannot open $DIR/slot$i" >&2; exit 1; }
    rc=0
    flock -n -E 75 "$fd" || rc=$?
    if [ "$rc" -eq 0 ]; then
      export HEAVY_SLOT=$i HEAVY_JOBS=$share
      export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$share} RUST_TEST_THREADS=${RUST_TEST_THREADS:-$share}
      "$@" {fd}<&- &
      child=$!
      trap 'kill -TERM "$child" 2>/dev/null' TERM INT HUP
      # wait returns early when a trapped signal arrives: wait again for the child's own status.
      while true; do
        rc=0
        wait "$child" || rc=$?
        kill -0 "$child" 2>/dev/null || exit "$rc"
      done
    fi
    exec {fd}<&-
    [ "$rc" -eq 75 ] || { echo "heavy: flock failed on $DIR/slot$i ($rc)" >&2; exit "$rc"; }
  done
  if [ -n "$deadline" ] && [ "$(date +%s)" -ge "$deadline" ]; then
    echo "heavy: every slot still busy after ${wait_for}s" >&2
    exit 75
  fi
  [ -n "$said" ] || { echo "heavy: all $SLOTS slots busy, waiting (heavy status)" >&2; said=1; }
  sleep 5
done
