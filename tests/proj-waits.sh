# What proj tells the stall watchdog a job waits on (modules/proj/waits.sh),
# from what rewatch would find of its latest turn: a turn in line or running
# is watched to its deadline (one waiting in line inside its run as one in
# line for a slot is), one that ended without saying what comes next or in a
# failure is due at once, an await to its deadline past proj-watch's, and a
# question for a person, a finished job, a stopped turn and an --owned job's
# end aren't watched at all. And what a running turn waits in line for: only
# a waiter that still holds its file, so one that died mid-wait never stops
# the turn's clock.
#
#   bash tests/proj-waits.sh modules/proj/waits.sh
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
export STALL_QUEUED=7200 AWAIT_HOURS=24 GRACE=600
T=$'\t'

# The facts, one "KEY VALUE..." argument each (spaces become tabs at the
# first two: an outcome keeps its own), and what the wait must be: "none", or
# a jq filter that must hold of it.
case_() {
  local want=$1 got facts='' f k rest
  shift
  for f in "$@"; do
    k=${f%% *} rest=${f#* }
    if [ "$k" = await ] || [ "$k" = wait ]; then rest=${rest/ /$T}; fi
    facts+="$k$T$rest"$'\n'
  done
  got=$(printf '%s' "$facts" | job_wait)
  if [ "$want" = none ]; then
    [ -z "$got" ] || { echo "FAIL: wanted nothing, got $got from: $*" >&2; exit 1; }
  else
    jq -e "$want" <<<"$got" >/dev/null || { echo "FAIL: wanted $want, got '$got' from: $*" >&2; exit 1; }
    # Always the watchdog's whole form.
    jq -e 'keys == ["due", "kick", "line", "link", "phase", "since", "what"] and (.since | type) == "number"' <<<"$got" >/dev/null ||
      { echo "FAIL: not the watchdog's form: $got" >&2; exit 1; }
  fi
}

base=("id web-1" "turn 2" "queued 1000" "slot 2000" "ended 3000" "deadline 5600")

# In line: watched for STALL_QUEUED from when it was started.
case_ '.since == 1000 and .due == 8200 and .kick == null and .line == true and .what == "proj job web-1" and (.phase | test("turn 2 .*waiting in line"))' \
  "${base[@]}" "state queued"
# An --owned job's turns wait in line like anyone's.
case_ '.due == 8200' "${base[@]}" "state queued" "owned 1"
# Running: to its deadline, and GRACE for proj to have stopped it.
case_ '.since == 2000 and .due == 6200 and .line == false and (.phase | test("turn 2 is running$"))' "${base[@]}" "state running"
# No timeout: nothing to hold it to.
case_ none "id web-1" "turn 2" "slot 2000" "state running"
# Waiting in line inside its run (its deadline moving on meanwhile): watched
# as a turn in line is, from when it started waiting, and says for what.
case_ '.since == 2500 and .due == 9700 and .line == false and (.phase | test("turn 2 is running, and waiting in line: the heavy-run lock \\(gate\\)$"))' \
  "${base[@]}" "state running" "wait 2500 the heavy-run lock (gate)"
case_ '.since == 2500 and .due == 9700' "id web-1" "turn 2" "slot 2000" "state running" "wait 2500 a lock"
# Ended, a wait it left is nothing to watch.
case_ none "${base[@]}" "state done" "end done" "wait 2500 a lock"

# Ended without saying what comes next, even when asked again: due at once.
case_ '.since == 3000 and .due == 3000 and .kick == null and (.phase | test("without saying what comes next"))' \
  "${base[@]}" "state done" "end undeclared"
# Ended in a failure, or an error: due at once too, and says which.
case_ '.due == 3000 and (.phase | test("ended \\(timed out after 4h\\)"))' "${base[@]}" "state timed out after 4h"
case_ '.due == 3000 and (.phase | test("ended \\(error\\)"))' "${base[@]}" "state error" "end done"
case_ '.due == 3000' "${base[@]}" "state failed"
# A question for a person, a finished job, and a turn that didn't have to say
# (a sandbox's, or one from before turns said) aren't watched.
case_ none "${base[@]}" "state done" "end ask"
case_ none "${base[@]}" "state done" "end done"
case_ none "${base[@]}" "state done"
# Nor is a turn proj stop ended, or how an --owned job's ended: its starter
# acts on that.
case_ none "${base[@]}" "state failed" "stopped 1"
case_ none "${base[@]}" "state done" "end undeclared" "stopped 1"
case_ none "${base[@]}" "state timed out after 1h" "owned 1"
case_ none "${base[@]}" "state done" "end undeclared" "owned 1"

# Awaiting GitHub: the longest wait sets the deadline, past proj-watch's own
# (AWAIT_HOURS), and its repo's proj-watch is kicked first.
case_ '.since == 4000 and .due == 4000 + 86400 + 600 and .kick == "proj-watch/c/d"
       and (.phase | test("a/b#1, c/d#2"))' \
  "${base[@]}" "state done" "end await" "await a/b#1 5000" "await c/d#2 4000"
# A line whose time can't be read is passed over, not the deadline.
case_ '.since == 5000 and .kick == "proj-watch/a/b"' "${base[@]}" "state done" "end await" "await a/b#1 5000" "await c/d#2 soon"
# Its awaits gone (proj-watch resumed it, or a stop took them): nothing.
case_ none "${base[@]}" "state done" "end await"

# What the job is about, when it says.
case_ '.link == "https://github.com/a/b/issues/7"' "${base[@]}" "state done" "end undeclared" "link https://github.com/a/b/issues/7"
case_ '.link == null' "${base[@]}" "state queued"

# What a running turn waits in line for (waiting), from its wait dir: the
# oldest file a live waiter holds locked.
w=$(mktemp -d)
saw() {
  local got
  got=$(waiting "$w")
  [ "$got" = "$1" ] || { echo "FAIL: waiting said '$got', wanted '$1'" >&2; exit 1; }
}
# Holds the file locked, as a waiter does, until it's killed.
hold() {
  ( exec 9<"$1" && flock -x 9 && exec sleep 600 ) &
  until ! flock -n "$1" true; do sleep 0.1; done
}
saw ''
# Held: when it was written, and its first line, control characters dropped.
printf 'the heavy-run lock\a (held by the gate)\nmore\n' >"$w/a"
touch -d @1000 "$w/a"
hold "$w/a"
a=$!
saw "1000${T}the heavy-run lock (held by the gate)"
# Two held: the oldest.
printf 'the gate lock\n' >"$w/b"
touch -d @2000 "$w/b"
hold "$w/b"
b=$!
saw "1000${T}the heavy-run lock (held by the gate)"
# The oldest's waiter dies mid-wait, its file left behind: no longer a wait,
# and gone.
kill "$a"
wait "$a" || true
saw "2000${T}the gate lock"
[ ! -e "$w/a" ] || { echo "FAIL: a dead waiter's file was kept" >&2; exit 1; }
# Just made, not locked yet: neither a wait nor removed.
: >"$w/c"
saw "2000${T}the gate lock"
[ -e "$w/c" ] || { echo "FAIL: a waiter's new file was removed before it could lock it" >&2; exit 1; }
# A line too long is cut to 200 characters; an empty one is a wait all the same.
kill "$b"
wait "$b" || true
printf '%0300d\n' 0 >"$w/d"
touch -d @3000 "$w/d"
hold "$w/d"
d=$!
saw "3000${T}$(printf '%0200d' 0)"
kill "$d"
wait "$d" || true
: >"$w/e"
touch -d @4000 "$w/e"
hold "$w/e"
e=$!
saw "4000${T}something"
kill "$e"
wait "$e" || true
saw ''
rm -rf "$w"

echo "proj-waits: ok"
