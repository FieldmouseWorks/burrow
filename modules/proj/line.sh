# The line for a slot: take_slot, and what it reads. The line is JOBS/.queue:
# one file per waiting turn, named so that sorting puts pipeline ones ahead,
# then --first ones, then the oldest, holding the waiting shell's pid. A slot
# is a lock on JOBS/.slots/N, and N.who says whose (class and project), so a
# turn that's killed frees its slot (and its place in line) by itself. pick
# (pick.sh) says who has a slot.
#
# One waiter looks at a time, under JOBS/.slots/.lock, and only those pick
# names can take a slot, so what a look costs comes out of every waiter's turn
# at the lock: with the line long, of how soon a free slot gets filled. So a
# look forks nothing per waiter (and git once per project: class), and it
# leaves all of pick's choice in JOBS/.slots/.next. For NEXT_FRESH seconds after
# that, any waiter it doesn't name has nothing to learn and skips its look. It's
# only ever a reason not to look: a slot is taken only by a look, under the lock.
# shellcheck disable=SC2034,SC2154 # jobs, spare (proj.sh) and the_place (pick.sh) when this is checked alone
NEXT_FRESH=2
# Whatever the note says, a waiter looks this often, which is as long as a job
# whose class changed can go on holding its old place in line.
LOOK_EVERY=60

# Whether a waiter with this place, which last looked at this time, should look
# now: it's never looked, or not lately, or there's no fresh note, or the note
# names it. The note is the time and each name, tab-separated, and may be caught
# half written: that costs a look, or a pass until the writer's done.
look_due() {
  local at='' names=''
  [ $((EPOCHSECONDS - $2)) -lt "$LOOK_EVERY" ] || return 0
  { IFS=$'\t' read -r at names <"$jobs/.slots/.next"; } 2>/dev/null || return 0
  [[ $at =~ ^[0-9]+$ ]] || return 0
  [ "$at" -le "$EPOCHSECONDS" ] && [ $((EPOCHSECONDS - at)) -lt "$NEXT_FRESH" ] || return 0
  [[ $'\t'$names$'\t' == *$'\t'"$1"$'\t'* ]]
}

# Leave what pick said (NAME N lines) as the note, for look_due.
note_next() {
  local name n note=$EPOCHSECONDS
  while IFS=$'\t' read -r name n; do
    [ -z "$name" ] || note+=$'\t'$name
  done <<<"$1"
  printf '%s\n' "$note" >"$jobs/.slots/.next"
}

# Whether the place at a path is a waiting shell's: its pid's cmdline is that
# job's _exec.
waiter_alive() {
  local pid='' fid=$2
  local -a args=()
  line_of pid "$1"
  [ -n "$pid" ] || return 1
  { mapfile -d '' -t args <"/proc/$pid/cmdline"; } 2>/dev/null || return 1
  [[ " ${args[*]} " == *" _exec $fid "* ]]
}

# A look, for the waiter at ME, job ID, class CLASS: what pick reads, into seen
# (the slots, then the line, once the places of shells that are gone are
# cleared), and busy if any slot is taken. Under the lock. A slot's lock can
# only be tried with flock(1), so that's the forks a look has, one per slot.
seen='' busy=''
look() {
  local me=$1 id=$2 own=$3 i fd who f name fid project
  seen='' busy=''
  classes=()
  for ((i = 1; i <= MAX_JOBS; i++)); do
    exec {fd}>"$jobs/.slots/$i"
    if flock -n "$fd"; then seen+="slot"$'\t'"$i"$'\t'"free"$'\n'
    else
      busy=1
      line_of who "$jobs/.slots/$i.who"
      seen+="slot"$'\t'"$i"$'\t'"${who:-$'-\t-'}"$'\n'
    fi
    exec {fd}>&-
  done
  for f in "$jobs/.queue"/*; do
    [ -e "$f" ] || continue
    name=${f##*/}; fid=${name#*-}; fid=${fid#*-}
    if [ "$f" = "$me" ]; then
      the_class=$own
      line_of project "$jobs/$id/project"
    elif waiter_alive "$f" "$fid"; then
      place "$name" "$jobs/$fid"
      line_of project "$jobs/$fid/project"
    else
      rm -f "$f"
      continue
    fi
    seen+="wait"$'\t'"$f"$'\t'"$the_class"$'\t'"$project"$'\n'
  done
}

# Wait in line for a slot, then hold it on fd $slot until this shell ends. The
# turn is first or normal, as it was started; its job's class (place, in
# pick.sh) is asked again at every look, and its place in line, its admission
# and its slot's N.who all go by that one answer.
take_slot() {
  local id=$1 n=$2 class=$3 me since got i lk new looked=0 project LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  since=$EPOCHSECONDS
  case $class in first) me=1 ;; *) me=2 ;; esac
  me="$jobs/.queue/$me-$(date +%s%N)-$id"
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # Waited long enough: among the --first ones now, by when it came.
    if [[ ${me##*/} == 2-* ]] && [ $((EPOCHSECONDS - since)) -ge "$MAX_WAIT" ]; then
      mv -f "$me" "${me%/*}/1-${me##*/2-}" 2>/dev/null || true
      me="${me%/*}/1-${me##*/2-}"
    fi
    if ! look_due "$me" "$looked"; then sleep 2; continue; fi
    # Where it stands now, and in the journal if that changed while it waits.
    classes=()
    place "${me##*/}" "$jobs/$id"
    class=$the_class new=$the_place
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
    # A slot is taken only under this lock, so one a look sees free stays free
    # until it's done. Another waiter's look may have come while this one
    # waited for it.
    exec {lk}>"$jobs/.slots/.lock"
    flock "$lk"
    if ! look_due "$me" "$looked"; then
      exec {lk}>&-
      sleep 2
      continue
    fi
    look "$me" "$id" "$class"
    looked=$EPOCHSECONDS
    got=$(printf '%s' "$seen" | pick)
    note_next "$got"
    i=$(picked "$me" <<<"$got")
    # Short of memory matters only while another job runs: with nothing
    # running, waiting frees nothing.
    if [ -n "$i" ] && { [ -z "$busy" ] || [ "$(spare)" -ge "$MIN_FREE" ]; }; then
      exec {slot}>"$jobs/.slots/$i"
      if flock -n "$slot"; then
        line_of project "$jobs/$id/project"
        printf '%s\t%s\n' "$class" "$project" >"$jobs/.slots/$i.who"
        # The note named this one: the next to go needn't wait it out, and
        # any others it named find theirs in the next look.
        : >"$jobs/.slots/.next"
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

# Every turn in line, in the order pick takes them (lead ones, then the rest):
# its job, class, how long it has waited, and its project. Only reads.
in_line() {
  local pass f name fid when wait project
  for pass in lead rest; do
    for f in "$jobs/.queue"/*; do
      [ -e "$f" ] || continue
      name=${f##*/}; fid=${name#*-}; when=${fid%%-*}; fid=${fid#*-}
      waiter_alive "$f" "$fid" || continue
      classes=()
      place "$name" "$jobs/$fid"
      if [ "$pass" = lead ]; then [ "$the_class" = lead ] || continue
      else [ "$the_class" != lead ] || continue
      fi
      line_of project "$jobs/$fid/project"
      wait=$((EPOCHSECONDS - when / 1000000000))
      printf '%-40s %-8s %4dm%02ds  %s\n' "$fid" "$the_class" $((wait / 60)) $((wait % 60)) "$project"
    done
  done
}

