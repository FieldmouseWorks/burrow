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
#   proj log ID [--turn N]      the job's latest answer, or turn N's (--raw for
#                               the full stream); fails when there's none
#   proj state ID               queued, running, done, error or failed
#   proj stop ID                end a job: its running turn and its waits
#   proj list                   projects you can run jobs in
#   proj await TARGET...        from inside a job: resume it once a PR or
#                               issue (URL or OWNER/REPO#N) changes
#
# Jobs run headless (claude -p) in SRC/PROJECT under the same managed
# settings as every session: no prompts, and an R2 command is refused rather
# than asked, so the job reports it and the operator brings it to the owner.
# Everything a job said is kept in JOBS/ID.
#
# At most MAX_JOBS turns run Claude at once, and a turn starts only with
# MIN_FREE MiB of memory to spare; the rest wait in line (queued), --first
# ones ahead, then oldest first, and a turn that has waited MAX_WAIT seconds
# counts as --first itself, so a stream of them can't starve it. A waiting
# turn is just this shell. Every turn runs in SLICE, which the machine caps,
# so jobs can't take memory from anything else, and gets TIMEOUT before it's
# stopped, so a stuck one can't hold its slot for good.
#
# A turn ends at its answer. Claude -p stays up after it while anything it
# left in the background runs (a CI watch, say), holding a slot to do
# nothing; so the answer stops it, and what it left running. A job waiting on
# GitHub says so with `proj await`, and `proj _watch` (a timer, ../proj.nix)
# gives it its next turn once what it waits on changes: checks finish, a
# review, comment or push comes in, or it's merged or closed. Nothing
# changed in AWAIT_HOURS, and it's told that instead.
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
# Jobs from before each turn kept its own files have the last turn's slot,
# outcome and answer unnumbered (a turn that ran across the update writes
# them there too, once it ends). They're turn N's if they're no older than its
# prompt; anything older is an earlier turn's, and goes.
adopt() {
  local d=$jobs/$1 n f
  n=$(cat "$d/turns" 2>/dev/null) || return 0
  for f in slot outcome result; do
    [ -e "$d/$f" ] || continue
    # A turn still running the old _exec reads its outcome back once it has
    # written it (to publish its waits), so that one stays until it has ended.
    if [ "$f" = outcome ] && ! [ "$d/prompt.$n" -nt "$d/$f" ] && ! gone "$(unit "$1")"; then continue; fi
    if [ ! -e "$d/$n.$f" ] && ! [ "$d/prompt.$n" -nt "$d/$f" ]; then
      mv -f "$d/$f" "$d/$n.$f" 2>/dev/null || true
    else
      rm -f "$d/$f"
    fi
  done
}

# The latest turn's: its slot and outcome are its own, named by its number,
# so one that never got as far as writing them is never read as the last.
# (status reads the answer after this, in the same line.)
state() {
  local n
  adopt "$1"
  n=$(cat "$jobs/$1/turns" 2>/dev/null || true)
  if systemctl --user -q is-active "$(unit "$1")"; then
    if [ -e "$jobs/$1/$n.slot" ]; then echo running; else echo queued; fi
  elif [ -s "$jobs/$1/$n.outcome" ]; then cat "$jobs/$1/$n.outcome"
  else echo failed
  fi
}

# A turn's answer, from its stream: every result with something to say, in
# order. Claude answers again each time a background task it left running
# reports back, so the last result can be a line about that task with the
# real answer one before it.
answer() { grep '"type":"result"' "$1" 2>/dev/null | jq -rs 'map(.result // "" | select(. != "")) | join("\n\n")'; }

# Swapped in whole, so a reader never finds it missing or half written.
set_turns() { echo "$2" >"$jobs/$1/turns.new" && mv -f "$jobs/$1/turns.new" "$jobs/$1/turns"; }

# Known not to be running: not "can't say".
gone() { case $(systemctl --user show -P ActiveState "$1" 2>/dev/null) in inactive | failed) return 0 ;; esac; return 1; }

start_turn() {
  local id=$1 msg=$2 first=$3 n
  local d=$jobs/$id
  n=$(($(cat "$d/turns" 2>/dev/null || echo 0) + 1))
  # The last turn's waits are the new turn's to clear (_exec), so a turn that
  # can't start, or a start_turn killed partway, leaves the job as it was.
  printf '%s\n' "$msg" >"$d/prompt.$n"
  set_turns "$id" "$n"
  systemctl --user reset-failed "$(unit "$id")" 2>/dev/null || true
  # Claude is what goes first when memory runs out, not whatever it's next to.
  if ! systemd-run --user --quiet --collect --unit="$(unit "$id")" \
    --slice="$SLICE" \
    --property=MemoryMax="$JOB_MEMORY_MAX" \
    --property=OOMScoreAdjust=500 \
    --property=WorkingDirectory="$(cat "$d/project")" \
    "$self" _exec "$id" "${first:-normal}"; then
    if [ "$n" = 1 ]; then rm -f "$d/turns"; else set_turns "$id" $((n - 1)); fi
    rm -f "$d/prompt.$n"
    echo "proj: turn $n of $id couldn't start" >&2
    return 1
  fi
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
  local id=$1 n=$2 first=$3 me since head f fid pid i fd num busy LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  since=$(date +%s)
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
    # Waited long enough: among the --first ones now, by when it came.
    if [[ ${me##*/} == 1-* ]] && [ $(($(date +%s) - since)) -ge "$MAX_WAIT" ]; then
      mv -f "$me" "${me%/*}/0-${me##*/1-}" && me="${me%/*}/0-${me##*/1-}"
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
          echo "$num" >"$jobs/$id/$n.slot"
          return
        fi
        exec {slot}>&-
      fi
    fi
    sleep 2
  done
}

# OWNER/REPO N from a PR or issue URL, or OWNER/REPO#N.
target() {
  local t=${1#https://github.com/}
  if [[ $t =~ ^([^/#]+/[^/#]+)/(pull|issues)/([0-9]+) ]]; then echo "${BASH_REMATCH[1]} ${BASH_REMATCH[3]}"
  elif [[ $t =~ ^([^/#]+/[^/#]+)#([0-9]+)$ ]]; then echo "${BASH_REMATCH[1]} ${BASH_REMATCH[2]}"
  else return 1
  fi
}

# What a job waiting on a PR or issue would want to hear about, as one line of
# JSON: a change in it is news. Checks count only once they've all finished,
# so a run of them wakes the job once. REST, not GraphQL, whose budget is
# shared with everything else under the login.
fingerprint() { # repo n
  local issue comments pr sha reviews status checks
  issue=$("$GH" api "repos/$1/issues/$2") || return
  # Comments on the issue or PR, but not proj-inbox's own (MARK): a job
  # awaiting its own issue would wake on every answer the inbox posts there.
  comments=$("$GH" api --paginate --slurp "repos/$1/issues/$2/comments?per_page=100" |
    jq --arg mark "$MARK" '[add // [] | .[] | select(.body | contains($mark) | not)] | length') || return
  if ! jq -e 'has("pull_request")' <<<"$issue" >/dev/null; then
    jq -c --argjson comments "$comments" '{state, comments: $comments}' <<<"$issue"
    return
  fi
  # Each answer is cut down to what's compared before it's passed on: a whole
  # one (a long PR body, hundreds of reviews) can be past what one argument
  # may hold.
  pr=$("$GH" api "repos/$1/pulls/$2" | jq -c '{state, merged, head: .head.sha, comments, review_comments}') || return
  sha=$(jq -r .head <<<"$pr")
  # Every page: a review or check past the first hundred is news too. Reviews
  # are counted by state, so a dismissed one is news as well as a new one.
  reviews=$("$GH" api --paginate --slurp "repos/$1/pulls/$2/reviews?per_page=100" |
    jq -c 'add // [] | group_by(.state) | map({(.[0].state): length}) | add // {}') || return
  status=$("$GH" api "repos/$1/commits/$sha/status" | jq -c '{total_count, state}') || return
  checks=$("$GH" api --paginate --slurp "repos/$1/commits/$sha/check-runs?per_page=100" |
    jq -r --argjson status "$status" '
      [.[].check_runs[]] as $runs |
      if any($runs[]; .status != "completed")
         or ($status.total_count > 0 and $status.state == "pending")
      then "running"
      else ([$runs[] | "\(.name)=\(.conclusion)"]
            + (if $status.total_count > 0 then ["status=\($status.state)"] else [] end)
            | sort | join(" "))
      end') || return
  jq -c --argjson comments "$comments" --argjson reviews "$reviews" --arg checks "$checks" \
    '. + {comments: $comments, reviews: $reviews, checks: $checks}' <<<"$pr"
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
      # A job's turns start one at a time, and _watch writes back a wait
      # only under the same lock (held until proj exits).
      exec {lk}>>"$jobs/$id/lock"
      flock "$lk"
      case $(state "$id") in queued | running) echo "$id is still $(state "$id")" >&2; exit 1 ;; esac
    fi
    start_turn "$id" "$task" "$first"
    echo "$id"
    ;;
  status)
    mkdir -p "$jobs"
    find "$jobs" -mindepth 1 -maxdepth 1 -type d -not -name '.*' -printf '%T@ %f\n' | sort -rn | head -n "${2:-15}" |
      while read -r _ id; do
        printf '%-40s %-8s %s%s%s\n' "$id" "$(state "$id")" \
          "$(sed 's/.*/[vm &] /' "$jobs/$id/agent" 2>/dev/null)" \
          "$(cut -f1 "$jobs/$id/await" 2>/dev/null | paste -sd' ' | sed 's/.\+/[awaits &] /')" \
          "$(head -c 100 "$jobs/$id/$(cat "$jobs/$id/turns" 2>/dev/null).result" 2>/dev/null | tr '\n' ' ')"
      done
    ;;
  log)
    id=${2:?usage: proj log ID [--turn N] [--raw]}
    shift 2
    n='' raw=''
    while [ $# -gt 0 ]; do
      case $1 in
        --raw) raw=1 ;;
        --turn) n=${2:?usage: proj log ID [--turn N] [--raw]}; shift ;;
        *) echo "usage: proj log ID [--turn N] [--raw]" >&2; exit 2 ;;
      esac
      shift
    done
    # The latest turn's only when no turn was asked for.
    [ -n "$n" ] || n=$(cat "$jobs/$id/turns")
    # A turn's answer is read from its own stream, so an earlier turn's is
    # still there once later ones have started.
    last=$(grep '"type":"result"' "$jobs/$id/$n.jsonl" 2>/dev/null | tail -n 1 || true)
    if [ -n "$raw" ]; then
      cat "$jobs/$id/$n.jsonl"
    elif [ -n "$last" ] && text=$(answer "$jobs/$id/$n.jsonl") && [ -n "$text" ]; then
      printf '%s\n' "$text"
    else
      # No result yet, or none with anything to say: an error's can hold
      # only its errors.
      if [ -z "$last" ] && [ "$n" = "$(cat "$jobs/$id/turns" 2>/dev/null)" ]; then
        echo "($(state "$id"); no answer yet)"
      else
        echo "(turn $n ended without an answer)"
      fi
      tail -n 3 "$jobs/$id/$n.jsonl" 2>/dev/null || true
      exit 1
    fi
    ;;
  state) state "${2:?usage: proj state ID}" ;;
  stop)
    id=${2:?usage: proj stop ID}
    [ -d "$jobs/$id" ] || { echo "no job $id" >&2; exit 1; }
    # Under the lock turns start under, so none starts between looking and
    # stopping. A stopped job is over: nothing it waited on resumes it.
    exec {lk}>>"$jobs/$id/lock"
    flock "$lk"
    u=$(unit "$id") rc=0
    gone "$u" || systemctl --user stop "$u" || rc=$?
    # Its waits go whatever the stop said. A failed stop is an error unless
    # the turn is known to be over: one that ended on its own between the
    # look and the stop is stopped all the same, and one systemd can't be
    # asked about may still be running.
    rm -f "$jobs/$id/await" "$jobs/$id/await.next"
    if [ "$rc" != 0 ] && ! gone "$u"; then
      echo "proj: couldn't stop $id" >&2
      exit "$rc"
    fi
    ;;
  list) find "$src" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort ;;
  await)
    # The baseline is taken now, so whatever changes from here on, even
    # before the turn has ended, is news. It's watched once the turn ends.
    id=${PROJ_JOB:?proj await is for a proj job, about what it waits on}
    shift
    [ $# -gt 0 ] || { echo "usage: proj await PR-OR-ISSUE-URL..." >&2; exit 2; }
    for t in "$@"; do
      target "$t" >/dev/null || { echo "not a GitHub PR or issue: $t" >&2; exit 2; }
    done
    for t in "$@"; do
      read -r repo num < <(target "$t")
      # No fingerprint (GitHub can't be asked just now): _watch takes it later.
      printf '%s\t%s\t%s\n' "$repo#$num" "$(date +%s)" "$(fingerprint "$repo" "$num" </dev/null || true)"
    done >>"$jobs/$id/await.next"
    echo "Noted. End your turn now: this job is resumed once $* changes."
    ;;
  _watch)
    # Every few minutes (../proj.nix): jobs waiting on GitHub whose PRs or
    # issues changed get their next turn. A line of JOBS/ID/await is
    # "OWNER/REPO#N <tab> since <tab> fingerprint".
    for f in "$jobs"/*/await; do
      [ -s "$f" ] || continue
      id=$(basename "$(dirname "$f")")
      case $(state "$id") in queued | running) continue ;; esac
      turn=$(cat "$jobs/$id/turns" 2>/dev/null) || continue
      # Gone already: another _watch (the timer's, or a hand-run one) has
      # started its next turn.
      waits=$(cat "$f" 2>/dev/null) || continue
      news='' lines=''
      while IFS=$'\t' read -r t since was; do
        [ -n "$t" ] || continue
        if ! read -r repo n < <(target "$t") || ! [[ $since =~ ^[0-9]+$ ]] ||
          { [ -n "$was" ] && ! jq -e 'type == "object"' <<<"$was" >/dev/null 2>&1; }; then
          # Not a line proj await writes (one edited by hand): the job hears
          # so rather than waiting on it for good.
          news+="- $t: not something proj can watch; no longer watched"$'\n'
          continue
        fi
        now=$(fingerprint "$repo" "$n" </dev/null) || now=''
        expired=$(($(date +%s) - since >= AWAIT_HOURS * 3600))
        if [ -n "$now" ] && [ -n "$was" ] && [ "$now" != "$was" ]; then
          news+="- $t: $(jq -rn --argjson was "$was" --argjson now "$now" '
            [$now | keys[] as $k | select($was[$k] != $now[$k]) | "\($k) \($was[$k] | tojson) → \($now[$k] | tojson)"] | join(", ")')"$'\n'
        elif [ "$expired" = 1 ] && [ -n "$now" ]; then
          news+="- $t: nothing changed in ${AWAIT_HOURS}h; no longer watched"$'\n'
        elif [ "$expired" = 1 ]; then
          # The deadline holds even when GitHub can't be read (down, or the
          # repo gone): the job hears that rather than waiting for good.
          news+="- $t: couldn't be read from GitHub, and ${AWAIT_HOURS}h have passed; no longer watched"$'\n'
        else
          lines+="$t"$'\t'"$since"$'\t'"${was:-$now}"$'\n'
        fi
      done <<<"$waits"
      # GitHub was asked without the lock, so a reply may have started a
      # turn meanwhile (that turn owns the job's waits), or proj stop ended
      # the job: either way these are dropped.
      exec {lk}>>"$jobs/$id/lock"
      flock "$lk"
      if [ "$(cat "$jobs/$id/turns")" != "$turn" ] || [ "$(cat "$f" 2>/dev/null)" != "$waits" ]; then
        :
      elif [ -n "$news" ]; then
        # The new turn clears what's left: if it still waits, it says so again.
        # One that can't start keeps its waits, so the next look tries again.
        start_turn "$id" "News on what you were waiting on:

$news
Carry on from there. Anything you left in the background last turn was stopped when it ended." "" || true
      else
        printf '%s' "$lines" >"$f.new" && mv -f "$f.new" "$f"
      fi
      exec {lk}>&-
    done
    ;;
  _exec)
    # One turn of a job, inside its own systemd unit.
    id=$2
    dir="$jobs/$id"
    n=$(cat "$dir/turns")
    # The last turn's waits, now that this one has started: this turn writes
    # its own to await.next, and await only once it ends.
    rm -f "$dir/await" "$dir/await.next"
    take_slot "$id" "$n" "$3"
    args=(-p --output-format stream-json --verbose)
    if [ -s "$dir/session" ]; then args+=(--resume "$(cat "$dir/session")"); fi
    : >"$dir/$n.jsonl"
    if [ -s "$dir/agent" ]; then
      timeout -k 1m "$TIMEOUT" agent sh "$(cat "$dir/agent")" claude "${args[@]}" <"$dir/prompt.$n" >"$dir/$n.jsonl" &
    else
      args+=(--append-system-prompt "This is proj job $id, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on GitHub (CI, reviews, merges) in the background or with sleep. Run \`proj await URL\` for each PR or issue you're still waiting on, then end your turn saying what you wait for: the job is resumed, with this conversation, once one of them changes (checks finish, a review, comment or push, merged or closed).")
      # A login shell, so the job sees the same PATH and env as an SSH login.
      # shellcheck disable=SC2016 # expanded by that shell
      PROJ_JOB=$id timeout -k 1m "$TIMEOUT" bash -lc 'exec claude "$@" <"$0"' "$dir/prompt.$n" "${args[@]}" >"$dir/$n.jsonl" &
    fi
    pid=$!
    # The answer ends the turn: a few seconds to exit by itself, then Claude
    # and whatever it left in the background are stopped (the unit takes the
    # rest when this shell ends), and the slot goes with them.
    if grep -qm1 '"type":"result"' < <(tail -n +1 -f --pid="$pid" "$dir/$n.jsonl"); then
      for _ in $(seq 10); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
      kill "$pid" 2>/dev/null || true
    fi
    rc=0
    wait "$pid" || rc=$?
    last=$(grep '"type":"result"' "$dir/$n.jsonl" | tail -n 1 || true)
    if [ -n "$last" ]; then
      rc=0
      jq -r '.session_id' <<<"$last" >"$dir/session"
      answer "$dir/$n.jsonl" >"$dir/$n.result"
      jq -r 'if .is_error then "error" else "done" end' <<<"$last" >"$dir/$n.outcome"
      # What it awaits, from the baseline `proj await` took; one line per
      # target, the first await of it in the turn.
      if [ "$(cat "$dir/$n.outcome")" = "done" ] && [ -s "$dir/await.next" ]; then
        # Swapped in whole, so _watch never reads half of it.
        awk -F '\t' '!seen[$1]++' "$dir/await.next" >"$dir/await.new" && mv -f "$dir/await.new" "$dir/await"
      fi
      rm -f "$dir/await.next"
    elif [ "$rc" -eq 124 ]; then
      echo "timed out after $TIMEOUT" >"$dir/$n.outcome"
    else
      echo failed >"$dir/$n.outcome"
    fi
    exit "$rc"
    ;;
  *) echo "usage: proj run|reply|status|log|state|stop|list" >&2; exit 2 ;;
esac
