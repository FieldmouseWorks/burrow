# proj: hand project work to a Claude running inside that project, as a
# background job, and check on it later. This is how the operator session (the
# one you talk to about the machine) does project work without leaving its own
# chat. SRC and JOBS come from ../proj.nix.
#
#   proj run PROJECT "task"     start a job; prints its id
#   proj run --vm PROJECT "task"  the same, in a fresh agent sandbox (agent.sh)
#   proj run --first PROJECT "task"  ahead of the line (someone is waiting on it)
#   proj reply [--first] ID "message"  continue that job's conversation
#   proj status                 recent jobs and their state
#   proj log ID                 the job's latest answer (--raw for the full stream)
#   proj state ID               queued, running, done, error or failed
#   proj stop ID                stop a running job
#   proj list                   projects you can run jobs in
#
# Jobs run headless (claude -p) in SRC/PROJECT under the same managed
# settings as every session: no prompts, and an R2 command is refused rather
# than asked, so the job reports it and the operator brings it to the owner.
# Everything a job said is kept in JOBS/ID.
#
# At most MAX_JOBS turns run Claude at once, and a turn starts only with
# MIN_FREE MiB of memory to spare; the rest wait in line (queued), --first
# ones ahead, then oldest first. A waiting turn is just this shell. Every turn
# runs in SLICE, which the machine caps, so jobs can't take memory from
# anything else, and gets TIMEOUT before it's stopped, so a stuck one can't
# hold its slot for good.
#
# With --vm the job runs in its own microVM on a clone of the project, where
# nothing is refused at all; the work comes back with `agent fetch` (the
# sandbox id is in JOBS/ID/agent and in `proj status`).

# systemctl --user needs the user's runtime dir, which `su` and cron don't set.
: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
export XDG_RUNTIME_DIR

src=$SRC
jobs=$JOBS
self=$(readlink -f "$0")

unit() { echo "proj-$(systemd-escape "$1").service"; }
state() {
  if systemctl --user -q is-active "$(unit "$1")"; then
    if [ -e "$jobs/$1/slot" ]; then echo running; else echo queued; fi
  elif [ -s "$jobs/$1/outcome" ]; then cat "$jobs/$1/outcome"
  else echo failed
  fi
}

start_turn() {
  local id=$1 msg=$2 first=$3 n
  n=$(($(cat "$jobs/$id/turns" 2>/dev/null || echo 0) + 1))
  printf '%s\n' "$msg" >"$jobs/$id/prompt.$n"
  echo "$n" >"$jobs/$id/turns"
  rm -f "$jobs/$id/result" "$jobs/$id/outcome" "$jobs/$id/slot"
  systemctl --user reset-failed "$(unit "$id")" 2>/dev/null || true
  # Claude is what goes first when memory runs out, not whatever it's next to.
  systemd-run --user --quiet --collect --unit="$(unit "$id")" \
    --slice="$SLICE" \
    --property=MemoryMax="$JOB_MEMORY_MAX" \
    --property=OOMScoreAdjust=500 \
    --property=WorkingDirectory="$(cat "$jobs/$id/project")" \
    "$self" _exec "$id" "${first:-normal}"
}

# Memory to spare, in MiB: what the kernel says is available, plus what the
# ZFS ARC would give back (the kernel doesn't count it).
spare() {
  local kb arc
  kb=$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)
  arc=0
  if [ -r /proc/spl/kstat/zfs/arcstats ]; then
    arc=$(awk '$1 == "size" { s = $3 } $1 == "c_min" { m = $3 } END { print (s > m ? int((s - m) / 1024) : 0) }' /proc/spl/kstat/zfs/arcstats)
  fi
  echo $(((kb + arc) / 1024))
}

# Wait in line for a slot, then hold it on fd $slot until this shell ends.
# The line is JOBS/.queue: one file per waiting turn, named so that sorting
# puts --first ones ahead and then the oldest, holding the waiting shell's pid.
# A slot is a lock on JOBS/.slots/N, so a turn that's killed frees its slot
# (and its place in line) by itself.
take_slot() {
  local id=$1 first=$2 me head f fid pid i fd num busy LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  me="$jobs/.queue/$([ "$first" = first ] && echo 0 || echo 1)-$(date +%s%N)-$id"
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # Published whole (a dot file, which the line skips, then renamed), so a
    # waiter clearing out dead places never sees it without its pid. Put
    # back if it went anyway.
    if [ ! -e "$me" ]; then
      echo "$$" >"${me%/*}/.${me##*/}.new"
      mv -f "${me%/*}/.${me##*/}.new" "$me"
    fi
    # The head of the line, once the places of shells that are gone are cleared.
    head=
    for f in "$jobs/.queue"/*; do
      [ -e "$f" ] || continue
      fid=${f##*/}; fid=${fid#*-}; fid=${fid#*-}
      pid=$(cat "$f" 2>/dev/null) || continue
      if [ -n "$pid" ] && tr '\0' ' ' 2>/dev/null <"/proc/$pid/cmdline" | grep -qF -- " _exec $fid "; then
        head=$f
        break
      fi
      rm -f "$f"
    done
    if [ "$head" = "$me" ]; then
      # A free slot, and whether any other is taken.
      slot='' busy=''
      for i in $(seq 1 "$MAX_JOBS"); do
        exec {fd}>"$jobs/.slots/$i"
        if ! flock -n "$fd"; then busy=1; exec {fd}>&-
        elif [ -z "$slot" ]; then slot=$fd num=$i
        else exec {fd}>&-
        fi
      done
      if [ -n "$slot" ]; then
        # Short of memory matters only while another job runs: with nothing
        # running, waiting frees nothing.
        if [ -z "$busy" ] || [ "$(spare)" -ge "$MIN_FREE" ]; then
          rm -f "$me"
          trap - EXIT TERM INT
          echo "$num" >"$jobs/$id/slot"
          return
        fi
        exec {slot}>&-
      fi
    fi
    sleep 2
  done
}

case "${1:-status}" in
  run | reply)
    cmd=$1 vm='' first=''
    shift
    while :; do
      case ${1:-} in
        --vm) [ "$cmd" = run ] || { echo "proj reply has no --vm" >&2; exit 2; }; vm=1 ;;
        --first) first=first ;;
        *) break ;;
      esac
      shift
    done
    if [ "$cmd" = run ]; then
      p=${1:?usage: proj run [--vm] [--first] PROJECT \"task\"}
      task=${2:?usage: proj run [--vm] [--first] PROJECT \"task\"}
      [ -d "$src/$p" ] || { echo "no such project: $src/$p" >&2; exit 1; }
      mkdir -p "$jobs"
      # Unique even when two jobs start in the same second.
      id=$(basename "$(mktemp -d "$jobs/$p-$(date +%Y%m%d-%H%M%S)-XXXX")")
      echo "$src/$p" >"$jobs/$id/project"
      if [ -n "$vm" ]; then agent new "$p" >"$jobs/$id/agent"; fi
    else
      id=${1:?usage: proj reply [--first] ID \"message\"}
      task=${2:?usage: proj reply [--first] ID \"message\"}
      [ -s "$jobs/$id/session" ] || { echo "$id has no session to continue (yet)" >&2; exit 1; }
      case $(state "$id") in queued | running) echo "$id is still $(state "$id")" >&2; exit 1 ;; esac
    fi
    start_turn "$id" "$task" "$first"
    echo "$id"
    ;;
  status)
    mkdir -p "$jobs"
    find "$jobs" -mindepth 1 -maxdepth 1 -type d -not -name '.*' -printf '%T@ %f\n' | sort -rn | head -n "${2:-15}" |
      while read -r _ id; do
        printf '%-40s %-8s %s%s\n' "$id" "$(state "$id")" \
          "$(sed 's/.*/[vm &] /' "$jobs/$id/agent" 2>/dev/null)" \
          "$(head -c 100 "$jobs/$id/result" 2>/dev/null | tr '\n' ' ')"
      done
    ;;
  log)
    id=${2:?usage: proj log ID [--raw]}
    n=$(cat "$jobs/$id/turns")
    if [ "${3:-}" = --raw ]; then
      cat "$jobs/$id/$n.jsonl"
    elif [ -s "$jobs/$id/result" ]; then
      cat "$jobs/$id/result"
    else
      echo "($(state "$id"); no answer yet)"
      tail -n 3 "$jobs/$id/$n.jsonl" 2>/dev/null || true
    fi
    ;;
  state) state "${2:?usage: proj state ID}" ;;
  stop) systemctl --user stop "$(unit "${2:?usage: proj stop ID}")" ;;
  list) find "$src" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort ;;
  _exec)
    # One turn of a job, inside its own systemd unit.
    id=$2
    dir="$jobs/$id"
    n=$(cat "$dir/turns")
    take_slot "$id" "$3"
    args=(-p --output-format stream-json --verbose)
    if [ -s "$dir/session" ]; then args+=(--resume "$(cat "$dir/session")"); fi
    rc=0
    if [ -s "$dir/agent" ]; then
      timeout -k 1m "$TIMEOUT" agent sh "$(cat "$dir/agent")" claude "${args[@]}" <"$dir/prompt.$n" >"$dir/$n.jsonl" || rc=$?
    else
      # A login shell, so the job sees the same PATH and env as an SSH login.
      # shellcheck disable=SC2016 # expanded by that shell
      timeout -k 1m "$TIMEOUT" bash -lc 'exec claude "$@" <"$0"' "$dir/prompt.$n" "${args[@]}" >"$dir/$n.jsonl" || rc=$?
    fi
    last=$(grep '"type":"result"' "$dir/$n.jsonl" | tail -n 1 || true)
    if [ -n "$last" ]; then
      jq -r '.session_id' <<<"$last" >"$dir/session"
      jq -r '.result // ""' <<<"$last" >"$dir/result"
      jq -r 'if .is_error then "error" else "done" end' <<<"$last" >"$dir/outcome"
    elif [ "$rc" -eq 124 ]; then
      echo "timed out after $TIMEOUT" >"$dir/outcome"
    else
      echo failed >"$dir/outcome"
    fi
    exit "$rc"
    ;;
  *) echo "usage: proj run|reply|status|log|state|stop|list" >&2; exit 2 ;;
esac
