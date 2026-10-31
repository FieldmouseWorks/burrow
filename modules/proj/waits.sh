# What one job waits on, for the stall watchdog (../watchdog.nix): rewatch's
# choice (proj.sh), on its own so the flake can check it. job_wait reads what
# facts found of the job's latest turn, one tab-separated line each:
# "id ID", "turn N", "state S" (proj state's), "owned 1" for a job whose
# starter acts on each turn's end itself (proj run --owned), "unstick 1" for
# one run --unstick (the stuck issue's), "stopped 1" for
# a turn proj stop ended, "end KIND" for how it said it ends (await, ask, done,
# or undeclared when it wouldn't say), "queued T", "slot T" and "ended T" for
# when it was started, took its slot and ended, "deadline T" for when a
# running turn's timeout is up as it stands (none without one), "wait T
# WHAT" for a running turn one of whose commands has waited in line since T
# (waiting's, below), "link URL" for what the job is about, and "await
# TARGET T" for each PR or issue it has awaited since T. It prints the job's
# wait as one line of JSON in the watchdog's form, {what, link, phase, since,
# due, kick, line, unstick}, or nothing when there's nothing to watch. line
# says it's waiting for a slot: another job would only join the line, so the
# watchdog gives it none. unstick says it's the job that takes the stuck
# issue, so the watchdog tells the owner of its stall rather than put it on
# that issue, where it would only be handed to itself.
#
# Watched: a turn waiting in line, for a slot or inside its run (its clock
# stands still meanwhile), for STALL_QUEUED seconds; one running, until its
# deadline and GRACE (proj stops it before then); one that ended without
# saying what comes next even when asked again, at once, since nothing resumes
# it; one that ended in a failure, for RESUME_GRACE from its end, since
# proj-inbox resumes an issue's job seconds later when a reply waits; one
# awaiting GitHub, for AWAIT_HOURS and GRACE (proj-watch resumes it by then,
# told nothing changed). Not watched: a turn that asks a person (their reply
# resumes it) or is done, one proj stop ended, and any ended turn of an
# --owned job.
job_wait() {
  local k a b id='' turn='' state='' owned='' unstick=false stopped='' end='' queued=0 slot=0 ended=0 deadline=0 link=''
  local since due phase kick='' line=false first='' targets='' t at waited='' what=
  while IFS=$'\t' read -r k a b; do
    case $k in
      id) id=$a ;;
      turn) turn=$a ;;
      state) state=$a ;;
      owned) owned=$a ;;
      unstick) unstick=true ;;
      stopped) stopped=$a ;;
      end) end=$a ;;
      queued) queued=$a ;;
      slot) slot=$a ;;
      ended) ended=$a ;;
      deadline) deadline=$a ;;
      wait) waited=$a what=$b ;;
      link) link=$a ;;
      await)
        # The longest wait sets the deadline, and its repo's proj-watch is
        # what's kicked first.
        [[ $b =~ ^[0-9]+$ ]] || continue
        targets+="${targets:+, }$a"
        if [ -z "$first" ] || [ "$b" -lt "$at" ]; then first=$a at=$b; fi
        ;;
    esac
  done
  case $state in
    queued)
      since=$queued due=$((queued + STALL_QUEUED)) line=true phase="turn $turn is waiting in line for a slot"
      [ "$unstick" = false ] || phase="turn $turn is waiting for the --unstick slot, which another --unstick turn holds"
      ;;
    running)
      if [ -n "$waited" ]; then
        since=$waited due=$((waited + STALL_QUEUED)) phase="turn $turn is running, and waiting in line: $what"
      else
        [ "$deadline" -gt 0 ] || return 0
        since=$slot due=$((deadline + GRACE)) phase="turn $turn is running"
      fi
      ;;
    *)
      [ -z "$stopped" ] && [ -z "$owned" ] || return 0
      if [ "$state" != "done" ]; then
        since=$ended due=$((ended + RESUME_GRACE)) phase="turn $turn ended ($state), and nothing resumed it"
      else
        case $end in
          undeclared)
            since=$ended due=$ended
            phase="turn $turn ended without saying what comes next (proj await, ask or done), even when asked again, so nothing resumes it"
            ;;
          await)
            [ -n "$first" ] || return 0
            since=$at due=$((at + AWAIT_HOURS * 3600 + GRACE)) kick="proj-watch/${first%#*}"
            phase="waiting on GitHub (proj await) for $targets; proj-watch resumes it once one changes, or after ${AWAIT_HOURS}h"
            ;;
          *) return 0 ;;
        esac
      fi
      ;;
  esac
  for t in "$since" "$due"; do [[ $t =~ ^[0-9]+$ ]] || return 1; done
  jq -nc --arg what "proj job $id" --arg link "$link" --arg phase "$phase" --argjson since "$since" \
    --argjson due "$due" --arg kick "$kick" --argjson line "$line" --argjson unstick "$unstick" \
    '{what: $what, link: (if $link == "" then null else $link end), phase: $phase, since: $since, due: $due,
      kick: (if $kick == "" then null else $kick end), line: $line, unstick: $unstick}'
}

# When a turn killed before it could write its outcome ended (what kills its
# whole cgroup takes everything in it along), from outside that cgroup: its
# _ended's first act. Only _ended writes it, and start_turn waits out the last
# turn's _ended before it writes a new turn, so this is always the turn that
# just ended, and no look at a turn being started can leave it on a live one.
# Kept once written, so nothing moves it.
# shellcheck disable=SC2154 # jobs is proj.sh's
mark_ended() {
  local d=$jobs/$1 n
  n=$(cat "$d/turns" 2>/dev/null) || return 0
  [ -e "$d/$n.outcome" ] || [ -e "$d/$n.ended" ] || : >"$d/$n.ended"
}

# What job_wait reads of a job's latest turn, from its dir and proj's state.
# A turn ended when it wrote its outcome, or when mark_ended saw it had. A look
# that finds neither is one in the moment before its _ended (now, which that
# then fixes), or at a turn being started, or one whose _ended never ran (the
# machine went down): its last write is all that's known, and looking again
# doesn't move it.
# shellcheck disable=SC2154 # jobs is proj.sh's
facts() {
  local d=$jobs/$1 n s t
  n=$(cat "$d/turns" 2>/dev/null) || return 1
  s=$(state "$1")
  printf 'id\t%s\nturn\t%s\nstate\t%s\n' "$1" "$n" "$s"
  [ ! -e "$d/owned" ] || printf 'owned\t1\n'
  [ ! -e "$d/unstick" ] || printf 'unstick\t1\n'
  [ ! -e "$d/$n.stopped" ] || printf 'stopped\t1\n'
  [ ! -s "$d/$n.end" ] || printf 'end\t%s\n' "$(cat "$d/$n.end")"
  [ ! -s "$d/link" ] || printf 'link\t%s\n' "$(cat "$d/link")"
  printf 'queued\t%s\n' "$(stat -c %Y "$d/prompt.$n" 2>/dev/null || echo 0)"
  [ ! -e "$d/$n.slot" ] || printf 'slot\t%s\n' "$(stat -c %Y "$d/$n.slot")"
  case $s in
    queued | running) ;;
    *)
      t=$(stat -c %Y "$d/$n.outcome" "$d/$n.ended" 2>/dev/null | head -n 1 || true)
      if [ -z "$t" ]; then
        if end_pending "$1"; then
          t=$(date +%s)
        else
          t=$(stat -c %Y "$d/$n.jsonl" "$d/prompt.$n" 2>/dev/null | head -n 1 || true)
        fi
      fi
      printf 'ended\t%s\n' "${t:-0}"
      ;;
  esac
  # Its deadline as it stands, to the second (none without a timeout): when
  # it took its slot and its timeout, moved on by any time it waited in
  # line. And what it waits in line for now, if anything (watch_run).
  t=$(cat "$d/$n.deadline" 2>/dev/null || true)
  [[ ! $t =~ ^[0-9]+$ ]] || printf 'deadline\t%s\n' "$((t / 1000000000))"
  [ ! -s "$d/$n.wait" ] || printf 'wait\t%s\n' "$(head -n 1 "$d/$n.wait")"
  [ ! -s "$d/await" ] || awk -F '\t' '{ print "await\t" $1 "\t" $2 }' "$d/await"
}

# A running turn's wait in line, from its wait dir (each turn's
# PROJ_WAIT_DIR): a command that waits in line for something shared, a
# machine-wide lock, creates a file there, writes one line saying what it
# waits for, and holds an exclusive flock(2) on the file while it waits,
# removing it once it has its turn. Prints "SINCE<tab>WHAT" for the oldest
# one still held (when it was written, its line, at most 200 characters), or
# nothing. A file nobody holds is a waiter's that died mid-wait (or left it
# behind), so it never stands for one: removed, once it's past the ten
# seconds a waiter has to lock the file it just made. One that holds its
# file and never gets its turn is the stall watchdog's (job_wait).
waiting() {
  local f t now best='' since=0 what
  now=$(date +%s)
  for f in "$1"/*; do
    [ -f "$f" ] || continue
    t=$(stat -c %Y "$f" 2>/dev/null) || continue
    if flock -n "$f" true 2>/dev/null; then
      [ $((now - t)) -lt 10 ] || rm -f "$f"
    elif [ -z "$best" ] || [ "$t" -lt "$since" ]; then
      best=$f since=$t
    fi
  done
  [ -n "$best" ] || return 0
  what=$(head -n 1 "$best" 2>/dev/null | tr -d '[:cntrl:]' | cut -c 1-200 || true)
  printf '%s\t%s\n' "$since" "${what:-something}"
}
