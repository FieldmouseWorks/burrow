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

# The command runs in a session of its own, so cancelling heavy (TERM, INT,
# HUP) cancels all of it, shells' workers too, and never the caller's group;
# the slot stays held until every process in it has stopped, and whatever is
# left GRACE seconds after the signal gets KILL. Its stdin is the caller's:
# bash would give a background command /dev/null.
child='' reaped='' cancelled='' kill_at=''
# Signals the command's group. Until setsid has made it one, the child is
# still bash's fork, which would take the signal as heavy's own trap and go on
# to start the command: wait for the group, or for the child to have died.
# Once reaped, its pid may be another's: only its group (which keeps the pid
# from being reused while it lasts) is left to signal.
stop() {
  local stat
  while [ -z "$reaped" ] && read -r stat 2>/dev/null </proc/"$child"/stat; do
    # shellcheck disable=SC2086 # its fields: state, ppid, pgrp
    set -- "$1" ${stat##*) }
    [ "$2" != Z ] && [ "$4" != "$child" ] || break
    sleep 0.01
  done
  kill -"$1" -- "-$child" 2>/dev/null || true
}
cancel() {
  cancelled=1
  [ -n "$kill_at" ] || kill_at=$(( $(now) + GRACE * 1000000 ))
  # Before the child's pid is known, run() signals it once it is.
  [ -z "$child" ] || stop TERM
}
# Whether the child is still running (not exited, reaped or not).
running() {
  local stat
  read -r stat 2>/dev/null </proc/"$child"/stat || return 1
  stat=${stat##*) }
  [ "${stat%% *}" != Z ]
}
# Reaps the child into rc once it has exited; returns 1 while it hasn't.
reap() {
  rc=0
  wait "$child" || rc=$?
  ! kill -0 "$child" 2>/dev/null || return 1
  reaped=1
}
run() {
  export HEAVY_SLOT=$1 HEAVY_JOBS=$share
  shift
  export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$share} RUST_TEST_THREADS=${RUST_TEST_THREADS:-$share}
  trap cancel TERM INT HUP
  exec {in}<&0
  setsid "$@" <&"$in" {in}<&- {fd}<&- &
  child=$!
  exec {in}<&-
  [ -z "$cancelled" ] || stop TERM
  # wait returns early when a trapped signal arrives: until then, wait for the
  # child's own status. Once cancelled, poll instead, so the KILL comes on time
  # even when the command ignores TERM or hangs in its handler.
  while [ -z "$cancelled" ]; do reap && break; done
  if [ -n "$cancelled" ]; then
    killed=''
    while [ -z "$reaped" ] || kill -0 -- "-$child" 2>/dev/null; do
      if [ -z "$reaped" ] && ! running; then reap || true; continue; fi
      [ -n "$killed" ] || [ "$(now)" -lt "$kill_at" ] || { stop KILL; killed=1; }
      sleep 0.1
    done
  fi
  exit "$rc"
}

# Takes the first free slot and runs the command in it; returns when none is free.
attempt() {
  for i in $(seq 1 "$SLOTS"); do
    exec {fd}<"$DIR/slot$i" || { echo "heavy: cannot open $DIR/slot$i" >&2; exit 1; }
    rc=0
    flock -n -E 75 "$fd" || rc=$?
    [ "$rc" -ne 0 ] || run "$i" "$@"
    exec {fd}<&-
    [ "$rc" -eq 75 ] || { echo "heavy: flock failed on $DIR/slot$i ($rc)" >&2; exit "$rc"; }
  done
}

# Microseconds, so a short --wait ends when it says.
now() { echo "${EPOCHREALTIME//[!0-9]/}"; }
deadline=''
[ -z "$wait_for" ] || deadline=$(( $(now) + wait_for * 1000000 ))
expired() {
  if [ -n "$deadline" ] && [ "$(now)" -ge "$deadline" ]; then
    echo "heavy: every slot still busy after ${wait_for}s" >&2
    exit 75
  fi
}

attempt "$@"
said=''
while true; do
  expired
  [ -n "$said" ] || { echo "heavy: all $SLOTS slots busy, waiting (heavy status)" >&2; said=1; }
  nap=1000000
  [ -z "$deadline" ] || [ $(( deadline - $(now) )) -ge "$nap" ] || nap=$(( deadline - $(now) ))
  [ "$nap" -le 0 ] || sleep "$(printf '%d.%06d' $(( nap / 1000000 )) $(( nap % 1000000 )))"
  expired
  attempt "$@"
done
