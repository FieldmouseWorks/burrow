# proj: hand project work to a Claude running inside that project, as a
# background job, and check on it later. This is how the operator session (the
# one you talk to about the machine) does project work without leaving its own
# chat. SRC and JOBS come from ../proj.nix.
#
#   proj run PROJECT "task"     start a job; prints its id
#   proj run --vm PROJECT "task"  the same, in a fresh agent sandbox (agent.sh)
#   proj reply ID "message"     continue that job's conversation
#   proj status                 recent jobs and their state
#   proj log ID                 the job's latest answer (--raw for the full stream)
#   proj stop ID                stop a running job
#   proj list                   projects you can run jobs in
#
# Jobs run headless (claude -p) in SRC/PROJECT under the same managed
# settings as every session: no prompts, and an R2 command is refused rather
# than asked, so the job reports it and the operator brings it to the owner.
# Everything a job said is kept in JOBS/ID.
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
  if systemctl --user -q is-active "$(unit "$1")"; then echo running
  elif [ -s "$jobs/$1/outcome" ]; then cat "$jobs/$1/outcome"
  else echo failed
  fi
}

start_turn() {
  local id=$1 msg=$2 n
  n=$(($(cat "$jobs/$id/turns" 2>/dev/null || echo 0) + 1))
  printf '%s\n' "$msg" >"$jobs/$id/prompt.$n"
  echo "$n" >"$jobs/$id/turns"
  rm -f "$jobs/$id/result" "$jobs/$id/outcome"
  systemctl --user reset-failed "$(unit "$id")" 2>/dev/null || true
  systemd-run --user --quiet --collect --unit="$(unit "$id")" \
    --property=WorkingDirectory="$(cat "$jobs/$id/project")" \
    "$self" _exec "$id"
}

case "${1:-status}" in
  run)
    vm=
    if [ "${2:-}" = --vm ]; then vm=1; shift; fi
    p=${2:?usage: proj run [--vm] PROJECT \"task\"}
    task=${3:?usage: proj run [--vm] PROJECT \"task\"}
    [ -d "$src/$p" ] || { echo "no such project: $src/$p" >&2; exit 1; }
    mkdir -p "$jobs"
    # Unique even when two jobs start in the same second.
    id=$(basename "$(mktemp -d "$jobs/$p-$(date +%Y%m%d-%H%M%S)-XXXX")")
    echo "$src/$p" >"$jobs/$id/project"
    if [ -n "$vm" ]; then agent new "$p" >"$jobs/$id/agent"; fi
    start_turn "$id" "$task"
    echo "$id"
    ;;
  reply)
    id=${2:?usage: proj reply ID \"message\"}
    msg=${3:?usage: proj reply ID \"message\"}
    [ -s "$jobs/$id/session" ] || { echo "$id has no session to continue (yet)" >&2; exit 1; }
    [ "$(state "$id")" != running ] || { echo "$id is still running" >&2; exit 1; }
    start_turn "$id" "$msg"
    echo "$id"
    ;;
  status)
    mkdir -p "$jobs"
    find "$jobs" -mindepth 1 -maxdepth 1 -type d -printf '%T@ %f\n' | sort -rn | head -n "${2:-15}" |
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
  stop) systemctl --user stop "$(unit "${2:?usage: proj stop ID}")" ;;
  list) find "$src" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort ;;
  _exec)
    # One turn of a job, inside its own systemd unit.
    id=$2
    dir="$jobs/$id"
    n=$(cat "$dir/turns")
    args=(-p --output-format stream-json --verbose)
    if [ -s "$dir/session" ]; then args+=(--resume "$(cat "$dir/session")"); fi
    rc=0
    if [ -s "$dir/agent" ]; then
      agent sh "$(cat "$dir/agent")" claude "${args[@]}" <"$dir/prompt.$n" >"$dir/$n.jsonl" || rc=$?
    else
      # A login shell, so the job sees the same PATH and env as an SSH login.
      bash -lc 'exec claude "$@" <"$0"' "$dir/prompt.$n" "${args[@]}" >"$dir/$n.jsonl" || rc=$?
    fi
    last=$(grep '"type":"result"' "$dir/$n.jsonl" | tail -n 1 || true)
    if [ -n "$last" ]; then
      jq -r '.session_id' <<<"$last" >"$dir/session"
      jq -r '.result // ""' <<<"$last" >"$dir/result"
      jq -r 'if .is_error then "error" else "done" end' <<<"$last" >"$dir/outcome"
    else
      echo failed >"$dir/outcome"
    fi
    exit "$rc"
    ;;
  *) echo "usage: proj run|reply|status|log|stop|list" >&2; exit 2 ;;
esac
