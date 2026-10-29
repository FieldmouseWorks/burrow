# What one job waits on, for the stall watchdog (../watchdog.nix): rewatch's
# choice (proj.sh), on its own so the flake can check it. It reads what
# rewatch found of the job's latest turn, one tab-separated line each:
# "id ID", "turn N", "state S" (proj state's), "owned 1" for a job whose
# starter acts on each turn's end itself (proj run --owned), "stopped 1" for
# a turn proj stop ended, "end KIND" for how it said it ends (await, ask, done,
# or undeclared when it wouldn't say), "queued T", "slot T" and "ended T" for
# when it was started, took its slot and ended, "limit S" for its timeout in
# seconds (0: none), "link URL" for what the job is about, and "await TARGET
# T" for each PR or issue it has awaited since T. It prints the job's wait as
# one line of JSON in the watchdog's form, {what, link, phase, since, due,
# kick}, or nothing when there's nothing to watch.
#
# Watched: a turn waiting in line, for STALL_QUEUED seconds; one running,
# until its timeout and GRACE (timeout(1) stops it before then); one that
# ended without saying what comes next even when asked again, or ended in a
# failure, at once, since nothing resumes it; one awaiting GitHub, for
# AWAIT_HOURS and GRACE (proj-watch resumes it by then, told nothing
# changed). Not watched: a turn that asks a person (their reply resumes it)
# or is done, one proj stop ended, and any ended turn of an --owned job.
job_wait() {
  local k a b id='' turn='' state='' owned='' stopped='' end='' queued=0 slot=0 ended=0 limit=0 link=''
  local since due phase kick='' first='' targets='' t at
  while IFS=$'\t' read -r k a b; do
    case $k in
      id) id=$a ;;
      turn) turn=$a ;;
      state) state=$a ;;
      owned) owned=$a ;;
      stopped) stopped=$a ;;
      end) end=$a ;;
      queued) queued=$a ;;
      slot) slot=$a ;;
      ended) ended=$a ;;
      limit) limit=$a ;;
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
    queued) since=$queued due=$((queued + STALL_QUEUED)) phase="turn $turn is waiting in line for a slot" ;;
    running)
      [ "$limit" -gt 0 ] || return 0
      since=$slot due=$((slot + limit + GRACE)) phase="turn $turn is running"
      ;;
    *)
      [ -z "$stopped" ] && [ -z "$owned" ] || return 0
      if [ "$state" != "done" ]; then
        since=$ended due=$ended phase="turn $turn ended ($state), and nothing resumes it"
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
    --argjson due "$due" --arg kick "$kick" \
    '{what: $what, link: (if $link == "" then null else $link end), phase: $phase, since: $since, due: $due,
      kick: (if $kick == "" then null else $kick end)}'
}
