#!/usr/bin/env bash
# The shell claude-rc-scan was before it was Rust, as modules/remote-control.nix
# had it inline (its Nix interpolations are the variables below, set by the
# test that runs it against the Rust one: tests/diff.rs). Kept as the
# reference that run compares against; nothing runs it otherwise.
set -o errexit -o nounset -o pipefail
: "${PROJECTS:?}" "${FLAKE:?}" "${TMUX:?}" "${SERVE:?}" "${IDLE:?}"
# Start a server for every project, stop the ones whose folder is gone or
# no longer a project, and restart the stale ones once they hold no
# session.
#
# A start while a scan runs (a path unit's, the booked timer's) only
# joins it, until it has stopped, so what changed meanwhile would go
# unseen: a session's status, a folder added or gone, a checkout's
# origin written (a clone writes it just after making the folder), a
# quiet spell's end. So each scan notes what it saw as it began and when
# its spells end, and as it stops (`claude-rc-scan done`) asks for a look after it
# (claude-rc-again@ID, `claude-rc-scan again ID`), which waits for it to
# be stopped and scans again if anything changed since or a spell is
# over. That look has a unit of its own, so no start joins it; and its
# start comes once this scan's is over, so nothing is lost after it
# either: a scan already running then has a look of its own to come.
sessions=$HOME/.claude/sessions
notes=$XDG_RUNTIME_DIR/claude-rc-scan
# Through links, as the scan reads them: the projects folder, a
# project's folder, its .git and its config may each be one.
seen() {
  { find "$sessions" -mindepth 1 -maxdepth 1 -printf '%p %T@\n' 2>/dev/null || true; } | sort
  { find -L "$PROJECTS" -mindepth 1 -maxdepth 1 -printf '%p\n' 2>/dev/null || true; } | sort
  { find -L "$PROJECTS" -mindepth 3 -maxdepth 3 -path '*/.git/config' -printf '%p %T@\n' 2>/dev/null || true; } | sort
}
case ${1:-} in
  done)
    exec systemctl --user start --no-block "claude-rc-again@$INVOCATION_ID.service"
    ;;
  again)
    note=$notes/$2
    wake=$(cat "$note.wake" 2>/dev/null || true)
    if [ "$(seen)" != "$(cat "$note.seen" 2>/dev/null || true)" ] ||
      { [ -n "$wake" ] && (( wake <= $(date +%s%3N) )); }; then
      systemctl --user start --no-block claude-rc-scan.service
    fi
    rm -f "$note.seen" "$note.wake"
    exit 0
    ;;
esac
mkdir -p "$notes"
note=$notes/$INVOCATION_ID
seen >"$note.seen"
# A project is a git checkout with an origin. Its .git is a folder: a
# linked worktree's is a file, and it's a checkout of a project that has
# its own server. Anything else would be a server with nothing to push
# to. The machine's own flake is served whatever it is.
#
# A folder that isn't one yet may become one where it stands, its
# origin written whenever that comes: a clone's a moment after it makes
# the folder, or after a pause, or `git init` and a remote added by
# hand. Nothing else changes in the projects folder then, so each such
# folder has a path unit on its .git/config while it isn't a project
# (claude-rc-origin@NAME), which scans again when that is written. A
# linked worktree never becomes one, and gets none.
flake=$(realpath -m "$FLAKE")
want=()
waiting=()
for d in "$PROJECTS"/*/; do
  [ -d "$d" ] || continue
  name=$(basename "$d")
  if [ "$(realpath "$d")" != "$flake" ] &&
    ! { [ -d "$d/.git" ] && git config --file "$d/.git/config" --get remote.origin.url >/dev/null; }; then
    if [ -d "$d/.git" ] || [ ! -e "$d/.git" ]; then
      waiting+=("$(systemd-escape --template claude-rc-origin@.path "$name")")
    fi
    continue
  fi
  want+=("$(systemd-escape --template claude-rc@.service "$name")")
done
# only GLOB UNIT...: start each UNIT, and stop every other GLOB matches.
only() {
  local glob=$1 unit w keep
  shift
  for unit in "$@"; do
    systemctl --user start --no-block "$unit"
  done
  while read -r unit _; do
    keep=
    for w in "$@"; do [ "$w" = "$unit" ] && keep=1; done
    [ -n "$keep" ] || systemctl --user stop --no-block "$unit"
  done < <(systemctl --user list-units --plain --no-legend --all "$glob")
}
only 'claude-rc@*.service' "${want[@]}"
only 'claude-rc-origin@*.path' "${waiting[@]}"
# A switch leaves running servers alone (restartIfChanged), so a server
# keeps the tmux and the claude-rc-serve it started with: its Claude
# Code, its permission mode, where its sessions run. One that runs any
# other is stale, and restarts here once none of its sessions is in
# use. A session (a child of its Claude) stays open as long as
# claude.ai keeps it, so being open is not being in use: Claude Code's
# ~/.claude/sessions/PID.json says what it's doing, and only one that
# is working, or did something in the last $IDLE minutes, holds the
# server. Nobody is cut off mid-turn, and an open but idle session
# doesn't keep a stale server forever. Its transcript stays on disk,
# and claude.ai resumes it on the new one.
#
# A held server's scan comes again when what holds it changes: a
# session's status (the path unit on ~/.claude/sessions), or the end of
# its quiet spell, which this scan books (claude-rc-scan.timer), at the
# latest of its sessions' and the soonest of the servers'. One that is
# working, or says nothing, is looked at again a quiet spell from now:
# it may end without a word.
now=$(date +%s%3N)
quiet=$(( $IDLE * 60000 ))
# held SESSION: whether it holds its server; `until` says until when,
# at the soonest.
held() {
  local f=$sessions/$1.json status at
  until=$(( now + quiet ))
  [ -r "$f" ] || return 0
  status=$(jq -r '.status // ""' "$f") || return 0
  at=$(jq -r '.statusUpdatedAt // 0' "$f") || return 0
  [ "$status" = busy ] && return 0
  until=$(( at + quiet ))
  (( now < until ))
}
wake=
for unit in "${want[@]}"; do
  pid=$(systemctl --user show -p MainPID --value "$unit")
  [ "$pid" != 0 ] && [ -r "/proc/$pid/cmdline" ] || continue
  [[ $(tr '\0' ' ' <"/proc/$pid/cmdline") == "$TMUX "*"$SERVE"* ]] && continue
  busy=
  last=0
  for rc in $(pgrep -P "$pid"); do
    # Any state but Z: a zombie is a session that already ended.
    for s in $(pgrep -P "$rc" -r D,I,R,S,T,t); do
      if held "$s"; then
        busy=1
        (( until <= last )) || last=$until
      fi
    done
  done
  if [ -z "$busy" ]; then
    systemctl --user restart --no-block "$unit"
  elif [ -z "$wake" ] || (( last < wake )); then
    wake=$last
  fi
done
# The wake-up: a timer of its own for this unit, gone once it fires.
systemctl --user stop claude-rc-scan.timer 2>/dev/null || true
echo "$wake" >"$note.wake"
if [ -n "$wake" ]; then
  # From the clock now, not the scan's start: a slow scan books no
  # later. In milliseconds: a timer never fires early, so the scan it
  # starts finds the spell over. A spell over already is the look
  # after this scan's to see.
  left=$(( wake - $(date +%s%3N) ))
  if (( left > 0 )); then
    systemd-run --user --quiet --unit=claude-rc-scan --on-active="${left}ms" \
      --timer-property=AccuracySec=1s --timer-property=RemainAfterElapse=no
  fi
fi
