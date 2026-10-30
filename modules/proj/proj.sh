# proj: hand project work to a Claude running inside that project, as a
# background job, and check on it later. This is how the operator session (the
# one you talk to about the machine) does project work without leaving its own
# chat. SRC and JOBS come from ../proj.nix.
#
#   proj run PROJECT "task"     start a job; prints its id
#   proj run --vm PROJECT "task"  the same, in a fresh agent sandbox (agent.sh)
#   proj run --first PROJECT "task"  ahead of the line (someone is waiting on it)
#   proj run --pipeline PROJECT "task"  work on the pipeline itself: in one of
#                               its own repos (PIPELINE, by the clone's GitHub
#                               origin), ahead of everything, and its own
#                               slots, and in a lead one (LEAD) ahead of the
#                               other pipeline work too; in any other, as
#                               --first, and status and the turn's journal
#                               say so (every turn of it, either way)
#   proj run --kick NAME PROJECT "task"  kick NAME (HANDLER or HANDLER/OWNER/REPO,
#                               one of burrow-events') each time a turn starts
#                               (takes its slot) and each time one ends
#   proj run --timeout DUR PROJECT "task"  each turn stopped after DUR (as
#                               timeout(1) reads it: 90m, 2h) instead of TIMEOUT
#   proj run --idle DUR PROJECT "task"  each turn stopped once its stream has
#                               said nothing for DUR: it's stuck
#   proj run --owned --kick NAME PROJECT "task"  NAME acts on each turn's end
#                               itself (the steward, the watchdog): the turn
#                               needn't say what comes next
#   proj run --link URL PROJECT "task"  what the job is about (its issue);
#                               empty for nothing
#   proj run --app NAME PROJECT "task"  the job acts on GitHub as the App NAME,
#                               one of burrow.github.apps, never as the user
#   proj reply [--first] ID "message"  continue that job's conversation
#   proj status                 recent jobs and their state
#   proj log ID [--turn N]      the job's latest answer, or turn N's (--raw for
#                               the full stream); fails when there's none
#   proj state ID               queued, running, done, error or failed
#   proj stop ID                end a job: its running turn and its waits
#   proj list                   projects you can run jobs in
#   proj await TARGET...        from inside a job: resume it once a PR or
#                               issue (URL or OWNER/REPO#N) changes
#   proj ask                    from inside a job: its answer is a question for
#                               a person, whose reply resumes it
#   proj done                   from inside a job: its work is finished
#
# Jobs run headless (claude -p) in SRC/PROJECT under the same managed
# settings as every session: no prompts, and an R2 command is refused rather
# than asked, so the job reports it and the operator brings it to the owner.
# Everything a job said is kept in JOBS/ID.
#
# At most MAX_JOBS turns run Claude at once, and a turn starts only with
# MIN_FREE MiB of memory to spare; the rest wait in line (queued), lead
# jobs' first, then the other pipeline jobs', then --first ones, then oldest
# first, and a turn that has waited MAX_WAIT seconds counts as --first itself,
# so a stream of them can't starve it. PIPELINE_SLOTS of the slots are for pipeline jobs in the
# pipeline's own repos alone, and no project's other jobs hold more than
# PER_PROJECT at once, and a waiting one of theirs is owed OTHER_SLOTS ahead of
# the pipeline's (pick.sh). A waiting turn is just this shell. Every turn
# runs in SLICE, which the machine caps, so jobs can't take memory from
# anything else, and gets TIMEOUT (or the job's own --timeout) before it's
# stopped, so a stuck one can't hold its slot for good. A job's --idle stops a
# stuck one sooner: a turn at work says something at least each time Claude's
# Bash tool stops a command (ten minutes at most), so one whose stream has
# been quiet longer than that is stuck, however long the work itself may take.
# Waiting its turn isn't being stuck: a command that waits in line inside a
# turn for something shared (a machine-wide lock) says so in the turn's
# PROJ_WAIT_DIR (waiting, in waits.sh, has how), and the turn's timeout
# stands still meanwhile, the stall watchdog holding that wait to
# STALL_QUEUED instead.
#
# A turn ends at its answer. Claude -p stays up after it while anything it
# left in the background runs (a CI watch, say), holding a slot to do
# nothing; so the answer stops it, and what it left running. That can be
# work it was waiting on, a build longer than one Bash call: once a turn, it
# hears it was stopped and answers again, in the same slot. A job waiting on
# GitHub says so with `proj await`, and `proj _watch` gives it its next turn
# once what it waits on changes: checks finish, a review, comment or push
# comes in, or it's merged or closed. Nothing changed in AWAIT_HOURS, and
# it's told that instead. burrow-events (../events.nix) starts _watch on the
# repo's webhook deliveries, when a turn that awaits it ends, and at the
# deadline _watch books in WAKE.
#
# Nothing else resumes a job, so every turn says what comes next, or it would
# wait for good on something nobody will tell it about (a background run, a
# notification): `proj await`, `proj ask` or `proj done`, before it answers.
# One that answers without saying hears so once, in the same slot, and
# answers again; still silent, it's a stall. Only a job whose starter acts on
# each turn's end itself (--owned: the steward's, the watchdog's) needn't
# say. With the stall watchdog on (../watchdog.nix), proj is one of its
# sources: WAITS has each job's wait (waits.sh) as of its last change, and
# the watchdog is kicked whenever that changes.
#
# With --vm the job runs in its own microVM on a clone of the project, where
# nothing is refused at all; the work comes back with `agent fetch` (the
# sandbox id is in JOBS/ID/agent and in `proj status`).
#
# With --app every turn runs in that App's env (../github.nix), its file
# from APPS, so git and gh act as the App: the machine's own work, not the
# user's. A turn whose App is gone fails rather than run as the user.

# systemctl --user needs the user's runtime dir, which `su` and cron don't set.
: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
export XDG_RUNTIME_DIR

src=$SRC
jobs=$JOBS
self=$(readlink -f "$0")

unit() { echo "proj-$(systemd-escape "$1").service"; }
# The env file of the GitHub App NAME, from APPS' NAME=FILE lines: nothing for
# one this machine hasn't.
app_env() { [[ $1 =~ ^[A-Za-z0-9_-]+$ ]] && sed -n "s/^$1=//p" <<<"$APPS" | head -n 1; }
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

# How turn N ended, for its log: its outcome, or failed when it has none and
# isn't going (its unit stopped, or killed with it, before it wrote one), as
# proj state says. Nothing while it's queued or running, a second run
# included, or for an earlier turn from before turns kept their outcomes (it
# has no slot either). Looked at under the lock turns start under, so no
# turn starts halfway through and has its state told as turn N's.
ended() {
  local d=$jobs/$1
  (
    flock -s 9
    adopt "$1"
    if [ -s "$d/$2.outcome" ]; then cat "$d/$2.outcome"
    elif [ "$2" = "$(cat "$d/turns" 2>/dev/null)" ]; then
      systemctl --user -q is-active "$(unit "$1")" || echo failed
    elif [ -e "$d/$2.slot" ]; then echo failed
    fi
  ) 9>>"$d/lock"
}

# A turn's answer, from its stream: every result with something to say, in
# order. Claude answers again each time a background task it left running
# reports back, so the last result can be a line about that task with the
# real answer one before it.
answer() { grep '"type":"result"' "$1" 2>/dev/null | jq -rs 'map(.result // "" | select(. != "")) | join("\n\n")'; }

# Swapped in whole, so a reader never finds it missing or half written.
set_turns() { echo "$2" >"$jobs/$1/turns.new" && mv -f "$jobs/$1/turns.new" "$jobs/$1/turns"; }

# A duration as timeout(1) reads it (90, 1.5h, 0.1s, 1d; 0 for none), in
# nanoseconds, rounded up; fails on anything else. Past about 30 years it's
# held there, as timeout(1) holds one too long for it: neither ever comes.
nanos() {
  awk -v d="$1" 'BEGIN {
    if (d !~ /^([0-9]+\.?[0-9]*|\.[0-9]+)([eE][-+]?[0-9]+)?[smhd]?$/) exit 1
    u = substr(d, length(d)); per = 1
    if (u == "m") per = 60; else if (u == "h") per = 3600; else if (u == "d") per = 86400
    if (u ~ /[smhd]/) d = substr(d, 1, length(d) - 1)
    s = d * per * 1e9; i = int(s); if (i < s) i++
    if (i > 1e18) i = 1e18
    printf "%.0f\n", i
  }'
}

# How a job waits on its own long work, which its answer would stop.
LONG="A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there."

# How a turn says what comes next: what resumes the job, if anything.
NEXT="\`proj await URL\` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), \`proj ask\` when your answer is a question only a person can settle (their reply resumes it), or \`proj done\` when the work is finished"

# Everything in this shell's unit but the shell: what a run left behind, in
# whatever process group, TERM first, then KILL. Fails if any of it is still
# there after that.
reap() {
  local cg p sig procs others
  cg=/sys/fs/cgroup$(sed -n 's/^0:://p' /proc/self/cgroup)
  for sig in TERM 0 0 0 0 0 0 0 0 0 KILL 0 0 0 0 0 0 0 0 0; do
    others=''
    mapfile -t procs <"$cg/cgroup.procs"
    for p in "${procs[@]}"; do
      [ "$p" = "$$" ] && continue
      others=1
      kill -"$sig" "$p" 2>/dev/null || true
    done
    [ -n "$others" ] || return 0
    sleep 1
  done
  return 1
}

# Kick each handler named on stdin (burrow-events'), once each.
kick() {
  [ -n "$KICK" ] || { cat >/dev/null; return 0; }
  sort -u | while read -r k; do
    [ -z "$k" ] || curl -fsS -m 5 -d '' "$KICK/$k" >/dev/null || echo "proj: couldn't kick $k" >&2
  done
}

# A run's watcher (pid $1, under timeout(1)), once a second until it's gone,
# sooner when its turn's deadline is closer: it stops the run (through that
# timeout(1)) once the deadline in $n.deadline has passed ($n.late), or once
# its stream has said nothing for the job's --idle ($n.quiet), and keeps the
# turn's wait in line ($n.wait, from waiting) for rewatch, telling the stall
# watchdog as one starts or ends. While one of its commands waits in line,
# the deadline moves on with the clock: waiting its turn for something
# shared isn't being stuck, and the watchdog holds a wait to STALL_QUEUED.
# Telling the watchdog goes on in the background, never waited for: its
# lock or a slow kick holds up neither the clock nor the end of the run.
watch_run() {
  local pid=$1 quiet=0 now last w was='' dl nap
  [ ! -s "$dir/idle" ] || quiet=$((($(nanos "$(cat "$dir/idle")") + 999999999) / 1000000000))
  last=$(date +%s%N)
  while kill -0 "$pid" 2>/dev/null; do
    now=$(date +%s%N)
    w=$(waiting "$waitdir")
    dl=$(cat "$dir/$n.deadline" 2>/dev/null || true)
    if [ -n "$w" ] && [ -n "$dl" ]; then
      dl=$((dl + now - last))
      echo "$dl" >"$dir/$n.deadline.new" && mv -f "$dir/$n.deadline.new" "$dir/$n.deadline"
    fi
    last=$now
    if [ "$w" != "$was" ]; then
      if [ -n "$w" ]; then printf '%s\n' "$w" >"$dir/$n.wait"; else rm -f "$dir/$n.wait"; fi
      was=$w
      rewatch "$id" &
    fi
    if [ -n "$dl" ] && [ "$now" -ge "$dl" ]; then
      : >"$dir/$n.late"
      kill "$pid" 2>/dev/null || true
      break
    fi
    if [ "$quiet" -gt 0 ] && [ $((now / 1000000000 - $(stat -c %Y "$dir/$n.jsonl"))) -ge "$quiet" ]; then
      : >"$dir/$n.quiet"
      kill "$pid" 2>/dev/null || true
      break
    fi
    nap=1
    if [ -n "$dl" ] && [ -z "$w" ] && [ $((dl - now)) -lt 1000000000 ]; then
      nap=0.$(printf %09d $((dl - now)))
    fi
    sleep "$nap"
  done
  if [ -n "$was" ]; then
    rm -f "$dir/$n.wait"
    rewatch "$id" &
  fi
}

# Known not to be running: not "can't say".
gone() { case $(systemctl --user show -P ActiveState "$1" 2>/dev/null) in inactive | failed) return 0 ;; esac; return 1; }

# What job_wait (waits.sh) reads of a job's latest turn.
facts() {
  local d=$jobs/$1 n t
  n=$(cat "$d/turns" 2>/dev/null) || return 1
  printf 'id\t%s\nturn\t%s\nstate\t%s\n' "$1" "$n" "$(state "$1")"
  [ ! -e "$d/owned" ] || printf 'owned\t1\n'
  [ ! -e "$d/$n.stopped" ] || printf 'stopped\t1\n'
  [ ! -s "$d/$n.end" ] || printf 'end\t%s\n' "$(cat "$d/$n.end")"
  [ ! -s "$d/link" ] || printf 'link\t%s\n' "$(cat "$d/link")"
  printf 'queued\t%s\n' "$(stat -c %Y "$d/prompt.$n" 2>/dev/null || echo 0)"
  [ ! -e "$d/$n.slot" ] || printf 'slot\t%s\n' "$(stat -c %Y "$d/$n.slot")"
  # When it ended: when it wrote its outcome, or for one stopped before it
  # could, when it last said anything.
  t=$(stat -c %Y "$d/$n.outcome" "$d/$n.jsonl" "$d/prompt.$n" 2>/dev/null | head -n 1 || true)
  printf 'ended\t%s\n' "${t:-0}"
  # Its deadline as it stands, to the second (none without a timeout): when
  # it took its slot and its timeout, moved on by any time it waited in
  # line. And what it waits in line for now, if anything (watch_run).
  t=$(cat "$d/$n.deadline" 2>/dev/null || true)
  [[ ! $t =~ ^[0-9]+$ ]] || printf 'deadline\t%s\n' "$((t / 1000000000))"
  [ ! -s "$d/$n.wait" ] || printf 'wait\t%s\n' "$(head -n 1 "$d/$n.wait")"
  [ ! -s "$d/await" ] || awk -F '\t' '{ print "await\t" $1 "\t" $2 }' "$d/await"
}

# The waits of the jobs named, brought up to date for the stall watchdog:
# JOBS/.waits/ID has a job's (job_wait's) while it has one, and WAITS all of
# them, which the watchdog reads. Called after each change to a job (a turn
# starting, taking its slot, ending, a stop), under one lock, so the last
# look at a job is the one that stands, and the watchdog is kicked when what
# it reads changed. A job whose last change never got here (the machine went
# down mid-turn) keeps the wait it had, whose deadline then catches it; one
# whose dir is gone has none. Without the watchdog there's no WAITS: nothing
# to do. Never the caller's failure: a turn goes on whether or not the
# watchdog hears.
rewatch() { waits "$@" || echo "proj: couldn't update the stall watchdog's waits for $*" >&2; }
waits() {
  [ -n "$WAITS" ] || return 0
  local id f wl items was LC_ALL=C
  mkdir -p "$jobs/.waits"
  exec {wl}>>"$jobs/.waits.lock"
  flock "$wl"
  for id in "$@"; do
    f=$jobs/.waits/$id
    if [ -d "$jobs/$id" ] && facts "$id" | job_wait >"$f.new" && [ -s "$f.new" ]; then
      mv -f "$f.new" "$f"
    else
      rm -f "$f" "$f.new"
    fi
  done
  for f in "$jobs/.waits"/*; do
    [ -e "$f" ] || continue
    [ -d "$jobs/${f##*/}" ] || rm -f "$f"
  done
  items=$(for f in "$jobs/.waits"/*; do
      [ -e "$f" ] || continue
      jq -c --arg id "${f##*/}" '{($id): .}' "$f" || true
    done | jq -cSs 'add // {}')
  was=$(jq -cS '.items // {}' "$WAITS" 2>/dev/null || echo '{}')
  jq -n --argjson items "$items" --argjson at "$(date +%s)" '{at: $at, items: $items}' >"$WAITS.new" &&
    mv -f "$WAITS.new" "$WAITS"
  if [ "$items" != "$was" ] && [ -n "$KICK" ]; then
    curl -fsS -m 5 -d '' "$KICK/watchdog" >/dev/null || echo "proj: couldn't kick watchdog" >&2
  fi
  exec {wl}>&-
}

start_turn() {
  local id=$1 msg=$2 first=$3 n i
  local d=$jobs/$id
  # The last turn reads as ended once it has written its outcome, but its
  # unit is still there while its _ended runs (telling the watchdog, kicking
  # proj-watch, which can start this turn), and no unit starts under that
  # name until it's gone. Waited out, for a minute at most, before this turn
  # is written, so _ended only ever reads the turn it ends as the latest.
  for i in $(seq 120); do
    systemctl --user reset-failed "$(unit "$id")" 2>/dev/null || true
    [ "$(systemctl --user show -P LoadState "$(unit "$id")" 2>/dev/null)" = loaded ] || break
    if [ "$i" = 120 ]; then
      echo "proj: $id's last turn is still ending, so its next can't start yet" >&2
      return 1
    fi
    sleep 0.5
  done
  n=$(($(cat "$d/turns" 2>/dev/null || echo 0) + 1))
  # The last turn's waits are the new turn's to clear (_exec), so a turn that
  # can't start, or a start_turn killed partway, leaves the job as it was.
  printf '%s\n' "$msg" >"$d/prompt.$n"
  set_turns "$id" "$n"
  # Claude is what goes first when memory runs out, not whatever it's next to.
  if ! systemd-run --user --quiet --collect --unit="$(unit "$id")" \
    --slice="$SLICE" \
    --property=MemoryMax="$JOB_MEMORY_MAX" \
    --property=OOMScoreAdjust=500 \
    --property=WorkingDirectory="$(cat "$d/project")" \
    --setenv=PROJ_ENDED="$id" \
    --property=ExecStopPost="$self _ended" \
    "$self" _exec "$id" "${first:-normal}"; then
    if [ "$n" = 1 ]; then rm -f "$d/turns"; else set_turns "$id" $((n - 1)); fi
    rm -f "$d/prompt.$n"
    echo "proj: turn $n of $id couldn't start" >&2
    return 1
  fi
  rewatch "$id"
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
# puts pipeline ones ahead, then --first ones, then the oldest, holding the
# waiting shell's pid. A slot is a lock on JOBS/.slots/N, and N.who says
# whose (class and project), so a turn that's killed frees its slot (and its
# place in line) by itself. pick (pick.sh) says who goes next. The turn is
# first or normal, as it was started; its job's class (place, in pick.sh)
# is asked again every pass, and its place in line, its admission and its
# slot's N.who all go by that one answer.
take_slot() {
  local id=$1 n=$2 class=$3 me since seen got f fid pid i fd lk busy new LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  since=$(date +%s)
  case $class in first) me=1 ;; *) me=2 ;; esac
  me="$jobs/.queue/$me-$(date +%s%N)-$id"
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # Waited long enough: among the --first ones now, by when it came.
    if [[ ${me##*/} == 2-* ]] && [ $(($(date +%s) - since)) -ge "$MAX_WAIT" ]; then
      mv -f "$me" "${me%/*}/1-${me##*/2-}" 2>/dev/null || true
      me="${me%/*}/1-${me##*/2-}"
    fi
    # Where it stands now, and in the journal if that changed while it waits.
    IFS=$'\t' read -r class new < <(place "${me##*/}" "$jobs/$id")
    if [ "$new" != "${me##*/}" ]; then
      if mv -f "$me" "${me%/*}/$new" 2>/dev/null; then echo "proj: it goes as $class now" >&2; fi
      me="${me%/*}/$new"
    fi
    # Published whole (a dot file, which the line skips, then renamed), so a
    # waiter clearing out dead places never sees it without its pid. Put
    # back if it went anyway.
    if [ ! -e "$me" ]; then
      echo "$$" >"${me%/*}/.${me##*/}.new"
      mv -f "${me%/*}/.${me##*/}.new" "$me"
    fi
    # One waiter looks at a time, and a slot is taken only under this lock,
    # so one it sees free stays free until it's done.
    exec {lk}>"$jobs/.slots/.lock"
    flock "$lk"
    seen='' busy=''
    for i in $(seq 1 "$MAX_JOBS"); do
      exec {fd}>"$jobs/.slots/$i"
      if flock -n "$fd"; then seen+="slot"$'\t'"$i"$'\t'"free"$'\n'
      else
        busy=1
        seen+="slot"$'\t'"$i"$'\t'"$(cat "$jobs/.slots/$i.who" 2>/dev/null || printf -- '-\t-')"$'\n'
      fi
      exec {fd}>&-
    done
    # The line, once the places of shells that are gone are cleared.
    for f in "$jobs/.queue"/*; do
      [ -e "$f" ] || continue
      fid=${f##*/}; fid=${fid#*-}; fid=${fid#*-}
      pid=$(cat "$f" 2>/dev/null) || continue
      if [ "$f" = "$me" ]; then
        seen+="wait"$'\t'"$f"$'\t'"$class"
        seen+=$'\t'"$(cat "$jobs/$id/project" 2>/dev/null)"$'\n'
      elif [ -n "$pid" ] && tr '\0' ' ' 2>/dev/null <"/proc/$pid/cmdline" | grep -qF -- " _exec $fid "; then
        seen+="wait"$'\t'"$f"$'\t'"$(place "${f##*/}" "$jobs/$fid" | cut -f1)"
        seen+=$'\t'"$(cat "$jobs/$fid/project" 2>/dev/null)"$'\n'
      else
        rm -f "$f"
      fi
    done
    got=$(printf '%s' "$seen" | pick)
    # Short of memory matters only while another job runs: with nothing
    # running, waiting frees nothing.
    if [ "${got%$'\t'*}" = "$me" ] && { [ -z "$busy" ] || [ "$(spare)" -ge "$MIN_FREE" ]; }; then
      i=${got##*$'\t'}
      exec {slot}>"$jobs/.slots/$i"
      if flock -n "$slot"; then
        printf '%s\t%s\n' "$class" "$(cat "$jobs/$id/project")" >"$jobs/.slots/$i.who"
        exec {lk}>&-
        rm -f "$me"
        trap - EXIT TERM INT
        echo "$i" >"$jobs/$id/$n.slot"
        return
      fi
      exec {slot}>&-
    fi
    exec {lk}>&-
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
    cmd=$1 vm='' first='' pipeline='' kick=() limit='' idle='' owned='' link='' app=''
    shift
    while :; do
      case ${1:-} in
        --vm) [ "$cmd" = run ] || { echo "proj reply has no --vm" >&2; exit 2; }; vm=1 ;;
        --kick)
          [ "$cmd" = run ] || { echo "proj reply has no --kick: the job's own are kept" >&2; exit 2; }
          [[ ${2:-} =~ ^[a-z0-9-]+(/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)?$ ]] || { echo "proj run --kick HANDLER[/OWNER/REPO]" >&2; exit 2; }
          kick+=("$2")
          shift
          ;;
        --timeout | --idle)
          [ "$cmd" = run ] || { echo "proj reply has no $1: the job's own is kept" >&2; exit 2; }
          [[ ${2:-} =~ ^[1-9][0-9]*[smhd]?$ ]] || { echo "proj run $1 DUR (a number, then s, m, h or d)" >&2; exit 2; }
          if [ "$1" = --timeout ]; then limit=$2; else idle=$2; fi
          shift
          ;;
        --owned) [ "$cmd" = run ] || { echo "proj reply has no --owned: the job's own is kept" >&2; exit 2; }; owned=1 ;;
        --link)
          [ "$cmd" = run ] || { echo "proj reply has no --link: the job's own is kept" >&2; exit 2; }
          # Empty is none.
          [[ ${2-x} =~ ^(https://[^[:space:]]+)?$ ]] || { echo "proj run --link URL (https://...)" >&2; exit 2; }
          link=$2
          shift
          ;;
        --app)
          [ "$cmd" = run ] || { echo "proj reply has no --app: the job's own is kept" >&2; exit 2; }
          [ -n "$(app_env "${2:-}")" ] || { echo "proj run --app NAME: one of this machine's GitHub Apps (burrow.github.apps)" >&2; exit 2; }
          app=$2
          shift
          ;;
        --first) first=first ;;
        --pipeline) [ "$cmd" = run ] || { echo "proj reply has no --pipeline: the job's own class is kept" >&2; exit 2; }; pipeline=1 ;;
        *) break ;;
      esac
      shift
    done
    if [ "$cmd" = run ]; then
      p=${1:?usage: proj run [--vm] [--first] [--pipeline] [--timeout DUR] [--idle DUR] [--kick NAME]... [--owned] [--link URL] [--app NAME] PROJECT \"task\"}
      task=${2:?usage: proj run [--vm] [--first] [--pipeline] [--timeout DUR] [--idle DUR] [--kick NAME]... [--owned] [--link URL] [--app NAME] PROJECT \"task\"}
      [ -d "$src/$p" ] || { echo "no such project: $src/$p" >&2; exit 1; }
      # Owned by what acts on each turn's end, which hears of it by its kick.
      [ -z "$owned" ] || [ ${#kick[@]} -gt 0 ] || { echo "proj run --owned needs a --kick: what acts on each turn's end" >&2; exit 2; }
      [ -z "$app" ] || [ -z "$vm" ] || { echo "proj run --app with --vm: a sandbox has no GitHub login to act with" >&2; exit 2; }
      mkdir -p "$jobs"
      # Unique even when two jobs start in the same second.
      id=$(basename "$(mktemp -d "$jobs/$p-$(date +%Y%m%d-%H%M%S)-XXXX")")
      echo "$src/$p" >"$jobs/$id/project"
      [ ${#kick[@]} = 0 ] || printf '%s\n' "${kick[@]}" >"$jobs/$id/kick"
      [ -z "$limit" ] || echo "$limit" >"$jobs/$id/timeout"
      [ -z "$idle" ] || echo "$idle" >"$jobs/$id/idle"
      # Work on the pipeline holds the pipeline's slots only in its own
      # repos' projects (class, in pick.sh), and says so when it won't.
      if [ -n "$pipeline" ]; then
        : >"$jobs/$id/pipeline"
        why=$(demoted "$jobs/$id")
        [ -z "$why" ] || echo "proj: $id $why" >&2
      fi
      [ -z "$owned" ] || : >"$jobs/$id/owned"
      [ -z "$link" ] || echo "$link" >"$jobs/$id/link"
      [ -z "$app" ] || echo "$app" >"$jobs/$id/app"
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
        st=$(state "$id")
        printf '%-40s %-8s %s%s%s%s%s%s%s\n' "$id" "$st" \
          "$(sed 's/.*/[vm &] /' "$jobs/$id/agent" 2>/dev/null)" \
          "$(sed 's/.*/[as &] /' "$jobs/$id/app" 2>/dev/null)" \
          "$(demoted "$jobs/$id" | sed 's/.\+/[&] /')" \
          "$([ "$st" != running ] || cut -f2 "$jobs/$id/$(cat "$jobs/$id/turns" 2>/dev/null).wait" 2>/dev/null | sed 's/.\+/[waiting in line: &] /')" \
          "$(cut -f1 "$jobs/$id/await" 2>/dev/null | paste -sd' ' | sed 's/.\+/[awaits &] /')" \
          "$(sed 's/^ask$/[asks] /; s/^undeclared$/[no next step] /; /^\[/!d' "$jobs/$id/$(cat "$jobs/$id/turns" 2>/dev/null).end" 2>/dev/null)" \
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
      # An answer, then an error, or a run told to answer again that ended
      # without one, its unit included: say what the turn ended on.
      outcome=$(ended "$id" "$n")
      if jq -e .is_error <<<"$last" >/dev/null; then
        printf '\n(turn %s ended in an error)\n' "$n"
        jq -c '.errors // empty' <<<"$last"
      elif [ -n "$outcome" ] && [ "$outcome" != "done" ]; then
        printf '\n(turn %s then ended: %s)\n' "$n" "$outcome"
      elif [ "$(cat "$jobs/$id/$n.end" 2>/dev/null)" = undeclared ]; then
        printf '\n(turn %s ended without saying what comes next, even when asked again: nothing resumes the job but a reply)\n' "$n"
      fi
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
    # Marked first, so the turn's end (_ended) already finds it stopped
    # rather than failed: nobody waits on it.
    n=$(cat "$jobs/$id/turns" 2>/dev/null || true)
    [ -z "$n" ] || : >"$jobs/$id/$n.stopped"
    gone "$u" || systemctl --user stop "$u" || rc=$?
    # Its waits go whatever the stop said. A failed stop is an error unless
    # the turn is known to be over: one that ended on its own between the
    # look and the stop is stopped all the same, and one systemd can't be
    # asked about may still be running.
    rm -f "$jobs/$id/await" "$jobs/$id/await.next"
    if [ "$rc" != 0 ] && ! gone "$u"; then
      [ -z "$n" ] || rm -f "$jobs/$id/$n.stopped"
      rewatch "$id"
      echo "proj: couldn't stop $id" >&2
      exit "$rc"
    fi
    rewatch "$id"
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
    echo await >"$jobs/$id/end.next"
    echo "Noted. End your turn now: this job is resumed once $* changes."
    ;;
  ask | done)
    # How the turn ends, said before it does: the last of proj await, ask
    # and done stands. Neither waits on GitHub any longer.
    id=${PROJ_JOB:?proj $1 is for a proj job, about how its turn ends}
    [ $# = 1 ] || { echo "usage: proj $1 (nothing more: your answer says it)" >&2; exit 2; }
    rm -f "$jobs/$id/await.next"
    echo "$1" >"$jobs/$id/end.next"
    case $1 in
      ask) echo "Noted. End your turn now with your question: the job is resumed when someone answers it." ;;
      done) echo "Noted. End your turn now with your answer." ;;
    esac
    ;;
  _watch)
    # `_watch [OWNER/REPO]`: jobs waiting on GitHub whose PRs or issues (in
    # that repo, or any) changed get their next turn. A line of JOBS/ID/await
    # is "OWNER/REPO#N <tab> since <tab> fingerprint". One pass at a time,
    # whoever started it (burrow-events, from before a restart too, or a
    # hand): two would each write WAKE from what they saw.
    exec {pass}>>"$WAKE.lock"
    flock "$pass"
    only=${2:-} retry=''
    for f in "$jobs"/*/await; do
      [ -s "$f" ] || continue
      id=$(basename "$(dirname "$f")")
      case $(state "$id") in queued | running) continue ;; esac
      turn=$(cat "$jobs/$id/turns" 2>/dev/null) || continue
      # Gone already: another _watch (burrow-events', or a hand-run one) has
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
        # GitHub's names are case-blind: the await's spelling needn't be the event's.
        if [ -n "$only" ] && [ "${repo,,}" != "${only,,}" ]; then
          lines+="$t"$'\t'"$since"$'\t'"$was"$'\n'
          continue
        fi
        # GitHub can't be asked just now: looked at again in five minutes.
        now=$(fingerprint "$repo" "$n" </dev/null) || { now=''; retry+="$repo"$'\t'$(($(date +%s) + 300))$'\n'; }
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
        # One that can't start keeps its waits, and is tried again in five
        # minutes.
        start_turn "$id" "News on what you were waiting on:

$news
Carry on from there. Anything you left in the background last turn was stopped when it ended." "" ||
          retry+=$(cut -f1 <<<"$waits" | sed 's/#.*//' | awk -v at=$(($(date +%s) + 300)) '{ print $0 "\t" at }')$'\n'
      else
        printf '%s' "$lines" >"$f.new" && mv -f "$f.new" "$f"
      fi
      exec {lk}>&-
    done
    # When each repo needs a look next, for burrow-events: the deadline of
    # every job's waits that's still to come, and the retries. One already
    # past is a turn that couldn't start (its retry stands for it) or one
    # this pass skipped as running, whose end kicks proj-watch itself. A pass
    # over one repo keeps the other repos' wake-ups (their retries too).
    if [ -n "$WAKE" ]; then
      { [ -z "$only" ] || jq -r --arg only "${only,,}" --argjson now "$(date +%s)" '
          to_entries[] | select((.key | ascii_downcase) != $only and (.value | type) == "number" and .value > $now)
          | "\(.key)\t\(.value)"' "$WAKE" 2>/dev/null || true
        { cat "$jobs"/*/await 2>/dev/null || true; } | awk -F '\t' -v h="$AWAIT_HOURS" -v now="$(date +%s)" '
          $2 ~ /^[0-9]+$/ && split($1, t, "#") == 2 && $2 + h * 3600 + 1 > now { print t[1] "\t" ($2 + h * 3600 + 1) }'
        printf '%s' "$retry"
      } | jq -Rn 'reduce (inputs | split("\t")) as [$r, $at] ({}; .[$r] = ([.[$r] // empty, ($at | tonumber)] | min))' \
        >"$WAKE.new"
      mv -f "$WAKE.new" "$WAKE"
    fi
    ;;
  _ended)
    # After each turn, however it ended (its unit's ExecStopPost): its wait
    # is the stall watchdog's to know, what the job was started to kick
    # (proj run --kick) gets a pass now, and so does
    # proj-watch for each repo the turn awaits, which then sees at once
    # anything that changed since the turn took its baseline. The job comes
    # in the unit's environment: systemd splits and expands a command line,
    # and a job's id is its project's folder name, spaces, `%` and all.
    id=${PROJ_ENDED:?proj _ended is for the end of a job turn, run by its unit}
    rewatch "$id"
    { cat "$jobs/$id/kick" 2>/dev/null || true
      cut -f1 "$jobs/$id/await" 2>/dev/null | sed 's/#.*//; s#^#proj-watch/#' || true
    } | kick
    ;;
  _exec)
    # One turn of a job, inside its own systemd unit.
    id=$2
    dir="$jobs/$id"
    n=$(cat "$dir/turns")
    # The last turn's waits, now that this one has started: this turn writes
    # its own to await.next, and await only once it ends; how it says it
    # ends goes to end.next.
    rm -f "$dir/await" "$dir/await.next" "$dir/end.next"
    # As its App, or not at all. Its env, read now, is what every run of the
    # turn starts in, so none starts as the user if the App goes while the
    # turn waits for a slot: it acts as an App that's gone, and reaches no
    # GitHub at all.
    appenv=''
    if [ -s "$dir/app" ]; then
      env=$(app_env "$(cat "$dir/app")")
      if [ -z "$env" ] || ! appenv=$(cat -- "$env" 2>/dev/null) || [ -z "$appenv" ]; then
        echo "no GitHub App $(cat "$dir/app") on this machine to act as" >"$dir/$n.outcome"
        exit 1
      fi
    fi
    # In the turn's journal too, where its wait would be looked into.
    why=$(demoted "$dir")
    [ -z "$why" ] || echo "proj: $why" >&2
    take_slot "$id" "$n" "$3"
    # It's at work now, which is news to what it was started to kick: a turn
    # can wait in line for hours, and the steward says which it is.
    { cat "$dir/kick" 2>/dev/null || true; } | kick
    limit=$(cat "$dir/timeout" 2>/dev/null || echo "$TIMEOUT")
    # One budget for the whole turn from the moment it took its slot,
    # however many runs it takes and the clearing up between them, to the
    # nanosecond; 0 is none. Its deadline is kept in $n.deadline, which the
    # time it waits in line moves on (watch_run).
    if ! total=$(nanos "$limit"); then
      echo "proj: the timeout $limit isn't a duration" >&2
      echo failed >"$dir/$n.outcome"
      exit 2
    fi
    rm -f "$dir/$n.deadline" "$dir/$n.wait" "$dir/$n.late"
    if [ "$total" -gt 0 ]; then
      taken=$(stat -c %.9Y "$dir/$n.slot")
      echo $((10#${taken/./} + total)) >"$dir/$n.deadline"
    fi
    # Where its commands say they're waiting in line (waiting, waits.sh).
    waitdir=$dir/$n.waits
    rm -rf "$waitdir"
    mkdir "$waitdir"
    rewatch "$id"
    # Whether its turns must say what comes next: an --owned job's starter
    # acts on each end itself, and a sandbox's Claude can't run proj.
    must=1
    [ ! -e "$dir/owned" ] && [ ! -s "$dir/agent" ] || must=''
    : >"$dir/$n.jsonl"
    prompt=$dir/prompt.$n
    while :; do
      args=(-p --output-format stream-json --verbose)
      if [ -s "$dir/session" ]; then args+=(--resume "$(cat "$dir/session")"); fi
      # Where this run's stream starts: an earlier run's answer isn't its.
      from=$(($(wc -l <"$dir/$n.jsonl") + 1))
      # Its deadline, looked at last thing before the run starts: past, and
      # no run starts, so the turn has timed out.
      if [ -s "$dir/$n.deadline" ] && [ "$(cat "$dir/$n.deadline")" -le "$(date +%s%N)" ]; then
        : >"$dir/$n.late"
        last='' rc=124
        break
      fi
      # Each run goes under timeout(1) with no limit of its own: watch_run
      # stops it through that, which stops all of it (TERM, then KILL a
      # minute on), as timeout(1) does at a limit.
      if [ -s "$dir/agent" ]; then
        timeout -k 1m 0 agent sh "$(cat "$dir/agent")" claude "${args[@]}" <"$prompt" >>"$dir/$n.jsonl" &
      else
        if [ -n "$must" ]; then
          args+=(--append-system-prompt "This is proj job $id, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on anything in the background or with sleep. Before you answer, say what comes next, since nothing else ever resumes this job: run $NEXT. Waiting on anything else (a background run, a notification) is waiting in this turn, in the foreground. $LONG")
        else
          args+=(--append-system-prompt "This is proj job $id, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on GitHub (CI, reviews, merges) in the background or with sleep. Run \`proj await URL\` for each PR or issue you're still waiting on, then end your turn saying what you wait for: the job is resumed, with this conversation, once one of them changes (checks finish, a review, comment or push, merged or closed). $LONG")
        fi
        # A login shell, so the job sees the same PATH and env as an SSH login,
        # and then its App's env, which nothing the login sets can undo; Claude
        # starts only once that has.
        # shellcheck disable=SC2016 # expanded by that shell
        PROJ_JOB=$id PROJ_WAIT_DIR=$waitdir PROJ_APP_ENV=$appenv timeout -k 1m 0 bash -lc 'eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec claude "$@" <"$0"' "$prompt" "${args[@]}" >>"$dir/$n.jsonl" &
      fi
      pid=$!
      watch_run "$pid" &
      dog=$!
      # The answer ends the turn: a few seconds to exit by itself, then Claude
      # and whatever it left in the background are stopped (the unit takes the
      # rest when this shell ends), and the slot goes with them.
      cut=''
      if grep -qm1 '"type":"result"' < <(tail -n +"$from" -f --pid="$pid" "$dir/$n.jsonl"); then
        for _ in $(seq 10); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
        if kill -0 "$pid" 2>/dev/null; then cut=1; kill "$pid" 2>/dev/null || true; fi
      fi
      rc=0
      wait "$pid" || rc=$?
      # Its watcher sees the run gone within a second, and clears up after it.
      wait "$dog" || true
      last=$(tail -n +"$from" "$dir/$n.jsonl" | grep '"type":"result"' | tail -n 1 || true)
      [ -z "$last" ] || jq -r '.session_id' <<<"$last" >"$dir/session"
      # Still up after its answer, it was waiting on something it left in the
      # background (a build past one Bash call), which went with it. Or it
      # answered without saying what comes next, and nothing would ever
      # resume it. Either way, once a turn, it's told so in the same
      # conversation and slot, and answers again. Waiting on GitHub is proj
      # await's. Only once all of the last run is gone, so none of it
      # overlaps the next or writes to its stream (and the next starts only
      # if the turn has time left); a sandbox's work is in the VM, out of
      # reach, so a --vm job isn't told.
      told=''
      [ -z "$cut" ] || told="Your answer ended the turn while something you left running in the background was still going, and it was stopped with it: nothing more from it reaches you, and the turn isn't over. If you were waiting on it, run it again and wait for it now."
      if [ -n "$must" ] && [ ! -s "$dir/end.next" ]; then
        told+="${told:+ }Your answer didn't say what comes next, and nothing resumes a job that doesn't. Say it now: run $NEXT. If you were waiting on anything else (a background run, a notification), nothing will tell you it's over: run it again and wait for it now, in the foreground. $LONG"
      elif [ -n "$cut" ]; then
        told+=" $LONG If you were waiting on GitHub, run \`proj await URL\` for it."
      fi
      if [ -n "$told" ] && [ "$prompt" = "$dir/prompt.$n" ] && [ ! -s "$dir/agent" ] &&
        jq -e '.is_error | not' <<<"$last" >/dev/null 2>&1 && reap; then
        printf '%s\n' "$told Then answer again." >"$dir/$n.cut"
        prompt=$dir/$n.cut
        # The next run's quiet counts from its start, and is its own.
        rm -f "$dir/$n.quiet"
        touch "$dir/$n.jsonl"
        # The last run's watcher telling the watchdog its wait was over
        # (still going, in the background) may have gone with the rest of
        # it: told again.
        rewatch "$id" &
        continue
      fi
      break
    done
    # The turn ends as its last run did; its answer is every run's.
    text=$(answer "$dir/$n.jsonl" || true)
    [ -z "$text" ] || printf '%s\n' "$text" >"$dir/$n.result"
    if [ -n "$last" ]; then
      rc=0
      # How it said it ends (before its outcome, which the watchdog's look
      # reads it with), or that it wouldn't when it must.
      end=$(cat "$dir/end.next" 2>/dev/null || true)
      [ -n "$end" ] || [ -z "$must" ] || end=undeclared
      if jq -e '.is_error | not' <<<"$last" >/dev/null && [ -n "$end" ]; then echo "$end" >"$dir/$n.end"; fi
      jq -r 'if .is_error then "error" else "done" end' <<<"$last" >"$dir/$n.outcome"
      # What it awaits, from the baseline `proj await` took; one line per
      # target, the first await of it in the turn.
      if [ "$(cat "$dir/$n.outcome")" = "done" ] && [ "$end" = await ] && [ -s "$dir/await.next" ]; then
        # Swapped in whole, so _watch never reads half of it.
        awk -F '\t' '!seen[$1]++' "$dir/await.next" >"$dir/await.new" && mv -f "$dir/await.new" "$dir/await"
      fi
      rm -f "$dir/await.next" "$dir/end.next"
    elif [ -e "$dir/$n.quiet" ]; then
      echo "went quiet for $(cat "$dir/idle")" >"$dir/$n.outcome"
    elif [ -e "$dir/$n.late" ]; then
      echo "timed out after $limit" >"$dir/$n.outcome"
      rc=124
    else
      echo failed >"$dir/$n.outcome"
    fi
    exit "$rc"
    ;;
  *) echo "usage: proj run|reply|status|log|state|stop|list|await|ask|done" >&2; exit 2 ;;
esac
