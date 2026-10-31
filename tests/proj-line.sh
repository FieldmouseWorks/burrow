# What a look at proj's line costs (modules/proj/line.sh): the forks it takes
# don't grow with the line, git runs at most once per project, and the note of
# who's next lets every other waiter skip its look, for as long as it's fresh.
# And where a turn stands in it: from when its job's work began (--since),
# with MAX_WAIT from when it came. And that a turn says it has its slot only
# under its job's lock, and stops saying it may be withdrawn under that lock
# before it's handed over.
#
#   bash tests/proj-line.sh modules/proj/pick.sh modules/proj/line.sh
# shellcheck disable=SC2154 # seen and busy, from look
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
# shellcheck source=/dev/null
. "$2"
export MAX_JOBS=6 PIPELINE_SLOTS=1 PER_PROJECT=4 PIPELINE="sam/burrow sam/box" LEAD="" FLAKE=/nowhere MAX_WAIT=99999 MIN_FREE=0
T=$'\t'
tmp=$(mktemp -d)
holder=''
trap '[ -z "$holder" ] || kill "$holder" 2>/dev/null || true; rm -rf "$tmp"' EXIT
export GIT_CEILING_DIRECTORIES=$tmp
jobs=$tmp/jobs
mkdir -p "$jobs/.queue" "$jobs/.slots" "$tmp/bin"
spare() { echo 1; }
fail() { echo "FAIL: $*" >&2; exit 1; }
in_queue() { find "$jobs/.queue" -mindepth 1 | wc -l; }

# git that says when it's run.
real_git=$(command -v git)
cat >"$tmp/bin/git" <<STUB
#!/bin/sh
echo "\$*" >>"$tmp/git.log"
exec "$real_git" "\$@"
STUB
chmod +x "$tmp/bin/git"
PATH=$tmp/bin:$PATH

clone() { "$real_git" init -q "$tmp/$1" && "$real_git" -C "$tmp/$1" remote add origin "$2"; }
clone burrow https://github.com/sam/burrow
clone box https://github.com/sam/box
clone web https://github.com/sam/web

# Waits with no child of its own, so killing it leaves nothing behind.
mkfifo "$tmp/never"
hold="exec 3<>$tmp/never; read -r -t 600 -u 3 _ || :"

# One shell stands in for every waiter: its cmdline has each one's _exec.
line_of_size() {
  local i j p args=()
  rm -f "$jobs/.queue"/*
  [ -z "$holder" ] || kill "$holder" 2>/dev/null || true
  for ((i = 0; i < $1; i++)); do
    j=job$i
    args+=(_exec "$j" 1 normal)
    mkdir -p "$jobs/$j"
    case $((i % 3)) in 0) p=burrow ;; 1) p=box ;; *) p=web ;; esac
    echo "$tmp/$p" >"$jobs/$j/project"
    : >"$jobs/$j/pipeline"
  done
  bash -c "$hold" proj "${args[@]}" &
  holder=$!
  # Its args are in its cmdline only once it's exec'd.
  until tr '\0' ' ' 2>/dev/null <"/proc/$holder/cmdline" | grep -q _exec; do sleep 0.05; done
  for ((i = 0; i < $1; i++)); do
    echo "$holder" >"$jobs/.queue/2-$((1000 + i))-job$i"
  done
}

# The forks one look takes, counted by the kernel. Inherited by the tracer:
# what's looked at runs as it does in take_slot.
forks() {
  local out=$tmp/forks.$1
  # shellcheck disable=SC2016 # for the shell it starts
  strace -f -qq -e trace=fork,vfork,clone,clone3 -o "$out" \
    bash -c '. "$1"; . "$2"; jobs=$3; look "$3/.queue/none" none normal' look "$PICK" "$LINE" "$jobs"
  grep -cE '^[0-9]+ +(fork|vfork|clone|clone3)\(' "$out"
}
PICK=$(realpath "$1") LINE=$(realpath "$2")
export PICK LINE

# Three projects, all --pipeline: git once for each, however many wait.
line_of_size 60
: >"$tmp/git.log"
look "$jobs/.queue/none" none normal
got=$(wc -l <"$tmp/git.log")
[ "$got" -le 3 ] || fail "a look ran git $got times for 3 projects"
[ "$(grep -c '^wait' <<<"$seen")" = 60 ] || fail "a look lost waiters: $seen"
grep -q "^wait$T$jobs/.queue/2-1000-job0${T}pipeline$T$tmp/burrow\$" <<<"$seen" || fail "job0 as a pipeline one in burrow: $seen"
grep -q "^wait$T$jobs/.queue/2-1002-job2${T}first$T$tmp/web\$" <<<"$seen" || fail "job2 as first in web: $seen"

# The forks don't grow with the line.
line_of_size 3
small=$(forks 3)
line_of_size 60
big=$(forks 60)
[ "$small" -gt 0 ] || fail "counted no forks at all, so the count says nothing"
[ "$big" = "$small" ] || fail "a look takes $small forks with 3 waiting, $big with 60"

# Shells that are gone lose their place; a pid that's another's does too.
line_of_size 2
echo $$ >"$jobs/.queue/2-3000-job9"
echo 999999999 >"$jobs/.queue/2-3001-job8"
: >"$jobs/.queue/2-3002-job7"
look "$jobs/.queue/none" none normal
[ "$(in_queue)" = 2 ] || fail "dead places stayed: $(in_queue)"
[ "$(grep -c '^wait' <<<"$seen")" = 2 ] || fail "dead places in the look: $seen"

# A taken slot reads as its N.who, or as not known; a free one as free.
exec {held}>"$jobs/.slots/1"
flock -n "$held"
exec {held2}>"$jobs/.slots/2"
flock -n "$held2"
printf 'normal\t/src/my web\n' >"$jobs/.slots/1.who"
look "$jobs/.queue/none" none normal
[ "$(sed -n 1p <<<"$seen")" = "slot${T}1${T}normal$T/src/my web" ] || fail "slot 1: $seen"
[ "$(sed -n 2p <<<"$seen")" = "slot${T}2$T-$T-" ] || fail "slot 2: $seen"
[ "$(sed -n 3p <<<"$seen")" = "slot${T}3${T}free" ] || fail "slot 3: $seen"
[ -n "$busy" ] || fail "two slots held, and not busy"
exec {held}>&- {held2}>&-

# Who looks: nobody else while the note is fresh, its name always, and everyone
# once it's old, torn or gone, or once they haven't looked in LOOK_EVERY.
note() { printf '%s\n' "$1" >"$jobs/.slots/.next"; }
now=$EPOCHSECONDS
due() { look_due "$1" "$2"; }
note "$now$T/q/next"
! due /q/other "$now" || fail "a fresh note, naming another, and it looks"
due /q/next "$now" || fail "a fresh note names it, and it doesn't look"
due /q/other 0 || fail "a waiter that never looked doesn't"
due /q/other $((now - LOOK_EVERY)) || fail "a waiter that hasn't looked in LOOK_EVERY doesn't"
note "$now$T/q/next$T/q/2-1-b"
due /q/2-1-b "$now" || fail "a fresh note names it second, and it doesn't look"
due /q/next "$now" || fail "a fresh note names it first, and it doesn't look"
! due /q/2-1 "$now" || fail "a fresh note names one its name starts, and it looks"
! due /q/other "$now" || fail "a fresh note naming two others, and it looks"
note "$now$T"
! due /q/other "$now" || fail "a fresh note that nobody goes, and it looks"
note "$((now - NEXT_FRESH))$T/q/next"
due /q/other "$now" || fail "an old note, and it doesn't look"
note "$((now + 100))$T/q/next"
due /q/other "$now" || fail "a note from the future, and it doesn't look"
note "12x"
due /q/other "$now" || fail "a torn note, and it doesn't look"
: >"$jobs/.slots/.next"
due /q/other "$now" || fail "an empty note, and it doesn't look"
rm "$jobs/.slots/.next"
due /q/other "$now" || fail "no note, and it doesn't look"

# What pick says becomes the note, every name of it; nothing, a note of nobody.
note_next "/q/a${T}1"$'\n'"/q/b${T}2"
due /q/a "$now" || fail "a note left out the first pick named: $(cat "$jobs/.slots/.next")"
due /q/b "$now" || fail "a note left out the second pick named: $(cat "$jobs/.slots/.next")"
! due /q/c "$now" || fail "a note named one pick didn't: $(cat "$jobs/.slots/.next")"
note_next ""
[ -z "$(cut -s -f2- "$jobs/.slots/.next")" ] || fail "nothing from pick, and a note of someone"
! due /q/a "$now" || fail "nothing from pick, and a waiter looks"
rm "$jobs/.slots/.next"

# Whole, with real shells: the line's head takes the free slot within seconds
# of it coming free, with the rest of the line skipping their looks; and it
# clears the note, so the next needn't wait it out.
rm -rf "${jobs:?}/.queue" "${jobs:?}/.slots"
mkdir -p "$jobs/.queue" "$jobs/.slots"
sleep 3 # so no place is left of the shell above
MAX_JOBS=1 PIPELINE_SLOTS=0
export -f look look_due note_next waiter_alive entry take_slot took_slot pick picked place class line_of github_repo origin listed flake_project spare
export jobs NEXT_FRESH LOOK_EVERY MAX_JOBS PIPELINE_SLOTS
exec {held}>"$jobs/.slots/1"
flock -n "$held"
waiter() {
  mkdir -p "$jobs/$1"
  echo "$tmp/web" >"$jobs/$1/project"
  bash -c 'id=$1; declare -A classes=(); take_slot "$id" 1 normal; echo "$EPOCHSECONDS" >"$jobs/$id/took"; eval "$2"' proj "$1" "$hold" _exec "$1" {held}>&- {held2}>&- >/dev/null 2>&1 &
  pids+=($!)
}
pids=()
trap '[ -z "$holder" ] || kill "$holder" 2>/dev/null || true; kill "${pids[@]}" 2>/dev/null || true; rm -rf "$tmp"' EXIT
waiter w0
until [ "$(in_queue)" = 1 ]; do sleep 0.05; done
for i in 1 2 3 4; do waiter "w$i"; done
until [ "$(in_queue)" = 5 ]; do sleep 0.05; done
sleep 3
freed=$EPOCHSECONDS
exec {held}>&-
for _ in $(seq 1 200); do [ -e "$jobs/w0/took" ] && break; sleep 0.05; done
[ -e "$jobs/w0/took" ] || fail "the head of the line didn't take the slot in 10s"
[ $(($(cat "$jobs/w0/took") - freed)) -le 6 ] || fail "the head took a free slot $(($(cat "$jobs/w0/took") - freed))s after it freed"
for i in 1 2 3 4; do [ ! -e "$jobs/w$i/took" ] || fail "w$i took a slot w0 holds"; done
[ "$(in_queue)" = 4 ] || fail "w0's place stayed in line"

# Two slots free at once: both go in a look or two, the next two in line, with
# the note's skipping holding up neither.
kill "${pids[@]}" 2>/dev/null || true
wait "${pids[@]}" 2>/dev/null || true
pids=()
rm -rf "${jobs:?}/.queue" "${jobs:?}/.slots" "${jobs:?}"/w?
mkdir -p "$jobs/.queue" "$jobs/.slots"
MAX_JOBS=2
exec {held}>"$jobs/.slots/1" {held2}>"$jobs/.slots/2"
flock -n "$held"
flock -n "$held2"
waiter w0
until [ "$(in_queue)" = 1 ]; do sleep 0.05; done
waiter w1
until [ "$(in_queue)" = 2 ]; do sleep 0.05; done
for i in 2 3 4; do waiter "w$i"; done
until [ "$(in_queue)" = 5 ]; do sleep 0.05; done
sleep 3
freed=$EPOCHSECONDS
exec {held}>&- {held2}>&-
for w in w0 w1; do
  for _ in $(seq 1 200); do [ -e "$jobs/$w/took" ] && break; sleep 0.05; done
  [ -e "$jobs/$w/took" ] || fail "$w didn't take a slot in 10s"
  [ $(($(cat "$jobs/$w/took") - freed)) -le 6 ] || fail "$w took a free slot $(($(cat "$jobs/$w/took") - freed))s after it freed"
done
for i in 2 3 4; do [ ! -e "$jobs/w$i/took" ] || fail "w$i took a slot ahead of the line"; done
[ "$(in_queue)" = 3 ] || fail "two took slots, and $(in_queue) are left of five"

# A turn says it has its slot (N.slot, proj state's running) only under its
# job's lock, which proj withdraw looks and stops under: with that held, the
# head of the line takes the free slot and waits to say so.
kill "${pids[@]}" 2>/dev/null || true
wait "${pids[@]}" 2>/dev/null || true
pids=()
rm -rf "${jobs:?}/.queue" "${jobs:?}/.slots" "${jobs:?}"/w?
mkdir -p "$jobs/.queue" "$jobs/.slots" "$jobs/w0"
MAX_JOBS=1
flock "$jobs/w0/lock" sleep 6 &
pids+=($!)
until ! flock -n "$jobs/w0/lock" true; do sleep 0.05; done
waiter w0
sleep 3
! flock -n "$jobs/.slots/1" true || fail "w0 took no slot while its job's lock was held"
[ ! -e "$jobs/w0/1.slot" ] || fail "w0 said it had its slot while its job's lock was held"
[ -e "$jobs/w0/1.withdrawable" ] || fail "w0 waits in line without saying proj withdraw may stop it"
for _ in $(seq 1 200); do [ -e "$jobs/w0/took" ] && break; sleep 0.05; done
[ "$(cat "$jobs/w0/1.slot" 2>/dev/null)" = 1 ] || fail "w0 didn't say it had slot 1 once its job's lock was let go"

# A turn handed over to the deployed proj stops saying it may be withdrawn
# first, under its job's lock: what it's handed to may take its slot without
# that lock (a proj from before took_slot, after a switch back to it). The one
# deployed here takes the hand-over and says whether the turn still did.
kill "${pids[@]}" 2>/dev/null || true
wait "${pids[@]}" 2>/dev/null || true
pids=()
rm -rf "${jobs:?}/.queue" "${jobs:?}/.slots" "${jobs:?}"/w?
mkdir -p "$jobs/.queue" "$jobs/.slots" "$jobs/w0" "$tmp/deployed"
cat >"$tmp/deployed/proj" <<STUB
#!/bin/sh
case \$1 in
  _handover_format) echo 1 ;;
  _exec) if [ -e "$jobs/\$2/1.withdrawable" ]; then echo marked; else echo unmarked; fi >"$jobs/\$2/handed" ;;
esac
STUB
chmod +x "$tmp/deployed/proj"
export -f successor unmark
export HANDOVER_FORMAT DEPLOYED=$tmp/deployed/proj self=/nowhere
flock "$jobs/w0/lock" sleep 6 &
pids+=($!)
until ! flock -n "$jobs/w0/lock" true; do sleep 0.05; done
waiter w0
sleep 3
[ ! -e "$jobs/w0/handed" ] || fail "w0 was handed over while its job's lock was held"
for _ in $(seq 1 200); do [ -e "$jobs/w0/handed" ] && break; sleep 0.05; done
[ "$(cat "$jobs/w0/handed" 2>/dev/null)" = unmarked ] || fail "w0 was handed over still saying proj withdraw may stop it"
export DEPLOYED=/nowhere

# in_line: all of it, in the order pick takes them, each with its wait.
LEAD=sam/burrow
kill "${pids[@]}" 2>/dev/null || true
wait "${pids[@]}" 2>/dev/null || true
line_of_size 4
got=$(in_line | awk '{print $1, $2}' | paste -sd,)
[ "$got" = "job0 lead,job3 lead,job1 pipeline,job2 first" ] || fail "in_line: $got"
[ "$(in_queue)" = 4 ] || fail "in_line changed the line"
# Its wait is from when its place was made, not its name's WHEN (the epoch
# here, as for a job started --since long ago).
in_line | sed -n 1p | grep -qE '^job0 +lead +0m[0-9]{2}s +.*/burrow$' || fail "in_line's row: $(in_line | sed -n 1p)"

# A turn's place (entry): one whose job was started --since a time (the
# steward's, from when its PR was opened) stands from then, ahead of turns that
# came before it but whose work began later, with --first ones ahead of the
# rest, and the names sort that way.
mkdir -p "$jobs/new" "$jobs/old-pr" "$jobs/asked"
new=$(entry new normal)
echo 1792800000 >"$jobs/old-pr/since"
old=$(entry old-pr normal)
asked=$(entry asked first)
[ "$old" = 2-1792800000000000000-old-pr ] || fail "old-pr's place from its --since: $old"
[[ $new =~ ^2-[0-9]{19}-new$ ]] || fail "new's place from now: $new"
got=$(printf '%s\n' "$new" "$old" "$asked" | LC_ALL=C sort | cut -d- -f3- | paste -sd ' ')
[ "$got" = "asked old-pr new" ] || fail "places in order: $got"

# MAX_WAIT goes by how long a turn has waited, not by its place's WHEN: one
# whose work began long ago (--since) waits among the normal turns until it has
# waited MAX_WAIT itself, then goes among the --first ones from the same WHEN.
kill "${pids[@]}" 2>/dev/null || true
wait "${pids[@]}" 2>/dev/null || true
pids=()
rm -rf "${jobs:?}/.queue" "${jobs:?}/.slots"
mkdir -p "$jobs/.queue" "$jobs/.slots" "$jobs/late"
MAX_JOBS=1
exec {held}>"$jobs/.slots/1"
flock -n "$held"
echo $((EPOCHSECONDS - 7200)) >"$jobs/late/since"
came=2-$(cat "$jobs/late/since")000000000-late
MAX_WAIT=4 waiter late
for _ in $(seq 1 200); do [ -e "$jobs/.queue/$came" ] && break; sleep 0.05; done
[ -e "$jobs/.queue/$came" ] || fail "late went in as $(find "$jobs/.queue" -mindepth 1 -printf '%f '), not $came"
for _ in $(seq 1 300); do [ -e "$jobs/.queue/1-${came#2-}" ] && break; sleep 0.05; done
[ -e "$jobs/.queue/1-${came#2-}" ] || fail "late wasn't among the --first ones past MAX_WAIT: $(find "$jobs/.queue" -mindepth 1 -printf '%f ')"
exec {held}>&-

echo "proj-line: ok"
