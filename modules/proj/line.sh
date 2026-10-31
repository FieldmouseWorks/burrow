# The line for a slot: take_slot, and what it reads. The line is JOBS/.queue:
# one file per waiting turn, named (entry) so that sorting puts pipeline ones
# ahead, then --first ones, then the oldest, holding the waiting shell's pid.
# A slot is a lock on JOBS/.slots/N, and N.who says whose (class and project),
# so a turn that's killed frees its slot (and its place in line) by itself.
# pick (pick.sh) says who has a slot. A job run --unstick waits in no line at
# all (unstick_slot), and one made so while it waits (proj unstick) leaves the
# line within a pass.
#
# One waiter looks at a time, under JOBS/.slots/.lock, and only those pick
# names can take a slot, so what a look costs comes out of every waiter's turn
# at the lock: with the line long, of how soon a free slot gets filled. So a
# look forks nothing per waiter (and git once per project: class), and it
# leaves all of pick's choice in JOBS/.slots/.next. For NEXT_FRESH seconds after
# that, any waiter it doesn't name has nothing to learn and skips its look. It's
# only ever a reason not to look: a slot is taken only by a look, under the lock.
#
# What a waiting turn decides with is the deployed proj's, not the one it
# started on: at its next look after a switch it execs that one, and picks up
# its place in line there, so a change to who goes first reaches everyone
# waiting and take_slot and pick are always one version. A running turn stays
# on its proj, but what it tells the stall watchdog is the deployed one's too
# (rewatch, proj.sh), so the format of a watch item has one owner. All that
# crosses versions is these: `proj _exec ID CLASS`, in the same pid, with the
# place left in JOBS/.queue under that pid and the look's lock held on fd 9
# until that proj is up; and `proj _rewatch ID...`. HANDOVER_FORMAT names them,
# and a proj that doesn't print the same one isn't handed anything (one from
# before this, or a switch back to it): the turn stays on the proj it has.
# shellcheck disable=SC2034,SC2154 # jobs, self, spare (proj.sh) and the_place (pick.sh) when this is checked alone
HANDOVER_FORMAT=1
DEPLOYED=/run/current-system/sw/bin/proj
NEXT_FRESH=2
# Whatever the note says, a waiter looks this often, which is as long as a job
# whose class changed can go on holding its old place in line.
LOOK_EVERY=60

# The deployed proj's store path, into the_successor, so the switch after this
# one can't move it from under what's run, if it isn't this one and can take a
# hand-over. Every waiter asks at every pass, so while the deployed proj is
# this one the answer takes no fork.
the_successor=''
successor() {
  local d
  the_successor=''
  [ -e "$DEPLOYED" ] && ! [ "$DEPLOYED" -ef "$self" ] || return 1
  d=$(readlink -f "$DEPLOYED") || return 1
  [ -x "$d" ] && [ "$("$d" _handover_format 2>/dev/null || true)" = "$HANDOVER_FORMAT" ] || return 1
  the_successor=$d
}

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

# A turn's place in line, the name JOBS/.queue has for it (PREFIX-WHEN-ID):
# 1 for a --first turn, 2 for any other (pick.sh's place says what that makes
# it now), and WHEN, in nanoseconds, from when its job's work began (proj run
# --since), or else from now. Every WHEN has the same number of digits, so
# sorting the names sorts them by when.
entry() { # id class
  local when
  line_of when "$jobs/$1/since"
  if [ -n "$when" ]; then when+="000000000"; else when=$(date +%s%N); fi
  case $2 in first) echo "1-$when-$1" ;; *) echo "2-$when-$1" ;; esac
}

# Wait in line for a slot, then hold it on fd $slot until this shell ends. The
# turn is first or normal, as it was started; its job's class (place, in
# pick.sh) is asked again at every look, and its place in line, its admission
# and its slot's N.who all go by that one answer.
take_slot() {
  local id=$1 n=$2 class=$3 me='' came='' f pid got i lk new looked=0 project LC_ALL=C
  mkdir -p "$jobs/.queue" "$jobs/.slots"
  # This turn says it has its slot only under its job's lock (took_slot), so
  # proj withdraw may stop it while it waits. On a proj from before that, it
  # can't be, never having said so: until it hands over to this one, or from
  # the moment this one hands it over (unmark).
  : >"$jobs/$id/$n.withdrawable"
  # The place a proj before this one left, if it handed over, and when that
  # place was made, which is when this turn came: MAX_WAIT goes by that, not
  # the name's WHEN, which for a job run --since is when its work began.
  for f in "$jobs/.queue"/*-"$id"; do
    line_of pid "$f"
    if [ "$pid" = "$$" ]; then
      me=$f
      came=$(date -r "$f" +%s 2>/dev/null) || came=''
    fi
  done
  [ -n "$me" ] || me="$jobs/.queue/$(entry "$id" "$class")"
  came=${came:-$EPOCHSECONDS}
  trap 'rm -f "$me" "${me%/*}/.${me##*/}.new"' EXIT
  trap 'exit 143' TERM INT
  while :; do
    # Made --unstick while it waits (proj unstick): it goes now, look or not.
    if [ -e "$jobs/$id/unstick" ]; then
      rm -f "$me"
      trap - EXIT TERM INT
      unstick_slot "$id" "$n"
      return
    fi
    # Waited long enough: among the --first ones now, in its place by WHEN.
    if [[ ${me##*/} == 2-* ]] && [ $((EPOCHSECONDS - came)) -ge "$MAX_WAIT" ]; then
      mv -f "$me" "${me%/*}/1-${me##*/2-}" 2>/dev/null || true
      me="${me%/*}/1-${me##*/2-}"
    fi
    # A switch since the last pass: on with the deployed proj, in this place,
    # whether or not a look is due (one the note skips can wait LOOK_EVERY).
    if successor; then
      unmark "$id" "$n"
      # Mid-exec its cmdline reads empty, and a look clearing out dead places
      # would take this one for gone: no look runs until it's through (_exec).
      exec 9>"$jobs/.slots/.lock"
      flock 9
      exec "$the_successor" _exec "$id" "$3"
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
        took_slot "$id" "$n" "$i"
        return
      fi
      exec {slot}>&-
    fi
    exec {lk}>&-
    sleep 2
  done
}

# A job run --unstick takes what the pipeline is stuck on, and that can be
# the line itself (pick handing out no slots, say, or every slot held by
# turns that wait on each other), so its turns never join it: they take a
# slot of their own past the MAX_JOBS the line shares, JOBS/.slots/unstick,
# and wait only for another --unstick turn to give it up. Nor do they wait
# for memory to spare: claude.slice, which the machine caps, holds this turn
# with the rest, and the job that finds why the machine is full can't wait
# for it to empty. Held on fd $slot, as take_slot's are.
unstick_slot() {
  local id=$1 n=$2 project
  mkdir -p "$jobs/.slots"
  exec {slot}>"$jobs/.slots/unstick"
  flock "$slot"
  line_of project "$jobs/$id/project"
  printf 'unstick\t%s\n' "$project" >"$jobs/.slots/unstick.who"
  took_slot "$id" "$n" unstick
}

# Say that job ID's turn N has slot S (N.slot, which proj state reads as
# running), under the job's lock: proj withdraw stops a turn only while it
# has none, and looks under that lock, so none takes one between its look
# and its stop. One withdrawn while it waits here is stopped with no slot.
took_slot() {
  local jl
  exec {jl}>>"$jobs/$1/lock"
  flock "$jl"
  echo "$3" >"$jobs/$1/$2.slot"
  exec {jl}>&-
}

# Say job ID's turn N may be withdrawn no more, under the job's lock, so none
# is stopped on this proj's word once another has it: before a hand-over, to a
# proj that may take its slot without that lock (one from before took_slot,
# after a switch back to it). One that can be withdrawn says so again itself,
# at the top of its take_slot.
unmark() {
  local jl
  exec {jl}>>"$jobs/$1/lock"
  flock "$jl"
  rm -f "$jobs/$1/$2.withdrawable"
  exec {jl}>&-
}

# Every turn in line, in the order pick takes them (lead ones, then the rest):
# its job, class, how long it has waited, and its project. Only reads. The wait
# is from when its place was made, not its name's WHEN, which for a job started
# --since is when its work began.
in_line() {
  local pass f name fid wait project
  for pass in lead rest; do
    for f in "$jobs/.queue"/*; do
      [ -e "$f" ] || continue
      name=${f##*/}; fid=${name#*-}; fid=${fid#*-}
      waiter_alive "$f" "$fid" || continue
      classes=()
      place "$name" "$jobs/$fid"
      if [ "$pass" = lead ]; then [ "$the_class" = lead ] || continue
      else [ "$the_class" != lead ] || continue
      fi
      line_of project "$jobs/$fid/project"
      wait=$(date -r "$f" +%s 2>/dev/null) || continue
      wait=$((EPOCHSECONDS - wait))
      printf '%-40s %-8s %4dm%02ds  %s\n' "$fid" "$the_class" $((wait / 60)) $((wait % 60)) "$project"
    done
  done
}

