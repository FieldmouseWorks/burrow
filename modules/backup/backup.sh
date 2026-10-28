# HOST-backup: the machine's data, off the machine. Each new snapshot of
# DATASETS whose name matches PATTERN goes to the restic repository REPO, read
# from the snapshot itself at the datasets' own paths, so the copy is one
# moment of the box and the next run reads and sends only what changed.
#
#   HOST-backup            back up the newest snapshot, if it isn't yet; once a
#                          day, also forget old backups, prune, read a share of
#                          the repository back and run the drill
#   HOST-backup status     the last backup, and what's in the repository
#   HOST-backup drill [PATH...]
#                          check the newest backup holds everything its
#                          snapshot had, then restore PATHs (default DRILL) to
#                          scratch and compare them with it, byte for byte
#   HOST-backup restic ARGS...
#                          restic itself, on this repository (to restore by
#                          hand: restic restore latest --target DIR --include PATH)
#
# Restic encrypts everything before it leaves, with the password in PASSWORD:
# without it the repository is noise, so its owner keeps a copy off the box.
# (Everything in capitals comes from ../backup.nix.)

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

self=$(readlink -f "$0")
state=$DIR
hold=burrow-backup
cmd=${1:-run}
[ "$#" = 0 ] || shift
# Where a snapshot's mount namespace keeps the snapshots before they go over
# their paths, and what it needs from the live box.
ns=/run/$NAME-ns

# Each dataset's mountpoint, in MP, and the datasets in ORDER, parents before
# their children (a parent's path is the shorter), so mounting one never
# hides another. From findmnt's JSON, which keeps a name as it is.
declare -A MP=()
order=()
mounts() {
  local ds m
  MP=()
  for ds in "${DATASETS[@]}"; do
    m=$(findmnt -l -J -o TARGET,FSROOT -S "$ds" || true)
    m=$(jq -r 'first(.filesystems[]? | select(.fsroot == "/") | .target) // empty' <<<"${m:-null}")
    [ -n "$m" ] || { echo "$ds isn't mounted" >&2; return 1; }
    # The backup runs from /nix/store and keeps what it needs live under
    # /run, so no snapshot may go over them.
    if [ "$m" = / ] || [[ /nix/store/ == "$m"/* || /run/ == "$m"/* ]]; then
      echo "$ds is mounted at $m, over the /nix/store or /run the backup runs from" >&2
      return 1
    fi
    MP[$ds]=$m
  done
  mapfile -t order < <(for ds in "${DATASETS[@]}"; do echo "${#MP[$ds]} $ds"; done | sort -n -k1,1 | cut -d' ' -f2)
}

# CMD with each dataset's snapshot SNAP mounted read-only over its own path, in
# a mount namespace of its own: restic sees the paths it saw last time, so it
# reads only the files that changed since. What CMD needs from the live box,
# whichever dataset it's in (the state directory with restic's cache and
# rclone's config, the password, a temp directory, and DIR with --bind DIR),
# is bound under $ns first, where no snapshot hides it, and found there.
in_snapshot() {
  unshare --mount --propagation private -- "$self" __in-snapshot "$@"
}

# The entries of the restic listing (ls --json) on stdin at or under any of
# the PATHs: NUL-terminated names (OUT names) or, one a line, JSON arrays of
# path, kind, mode, owner, size, times and inode (OUT fields).
under() {
  local out=$1
  shift
  jq -j --arg out "$out" --args '
      select(.message_type == "node") | .path as $p
      | select(any($ARGS.positional[] | if . == "/" then "" else rtrimstr("/") end;
          . as $a | $p == $a or ($p | startswith($a + "/"))))
      | if $out == "names" then $p + "\u0000"
        else ([.path, .type, .mode, .uid, .gid, .size, .mtime, .ctime, .inode] | tojson) + "\n" end' "$@"
}

# Kind, mode and owner of each NUL-separated ./PATH in $2 under $1, and size
# and mtime of the regular files among them.
meta() {
  (cd "$1" && xargs -0r -a "$2" bash -c \
    'exec find -P "$@" -maxdepth 0 \( -type f -printf "%p %y %m %U %G %s %T@\n" -o -printf "%p %y %m %U %G\n" \)' find-meta) | LC_ALL=C sort
}

# (In the snapshot's namespace.) Each PATH restored under BASE/tree must be
# exactly what the backup holds there (BASE/expected, its listing), and what
# the snapshot had: the same entries, bytes, kinds, modes, owners, sizes and
# mtimes. Names only, never contents, go to the log.
compare() {
  local base=$1 p work bad=0
  shift
  work=$(mktemp -d)
  for p in "$@"; do
    [ "$p" = / ] || p=${p%/}
    P=$p awk 'BEGIN { RS = ORS = "\0"; p = ENVIRON["P"] } $0 == p || index($0, p "/") == 1' "$base/expected" | LC_ALL=C sort -z >"$work/want"
    if [ ! -s "$work/want" ]; then
      echo "drill: $p isn't in the backup"; bad=1; continue
    fi
    { find -P "$base/tree$p" -print0 2>/dev/null || true; } |
      R=$base/tree awk 'BEGIN { RS = ORS = "\0"; r = ENVIRON["R"] } { print substr($0, length(r) + 1) }' | LC_ALL=C sort -z >"$work/have"
    if ! cmp -s "$work/want" "$work/have"; then
      echo "drill: restoring $p didn't bring back what the backup holds (< only in the backup, > only restored):"
      { LC_ALL=C comm -z -23 "$work/want" "$work/have" | tr '\0' '\n' | sed 's/^/< /'
        LC_ALL=C comm -z -13 "$work/want" "$work/have" | tr '\0' '\n' | sed 's/^/> /'; } | head -20
      bad=1; continue
    fi
    # What the snapshot has beyond the backup, the backup's own rules left
    # out (the drill checked that first).
    out=$(LC_ALL=C diff -rq --no-dereference "$p" "$base/tree$p" 2>&1 | grep -vF "Only in $p" || true)
    if [ -n "$out" ]; then
      echo "drill: $p as restored isn't what the snapshot had:"; head -20 <<<"$out"; bad=1; continue
    fi
    awk 'BEGIN { RS = ORS = "\0" } { print "." $0 }' "$work/want" >"$work/rel"
    if ! diff <(meta / "$work/rel") <(meta "$base/tree" "$work/rel") >"$work/meta"; then
      echo "drill: the metadata of $p as restored isn't what the snapshot had (< snapshot, > restored):"; head -20 "$work/meta"; bad=1; continue
    fi
    echo "drill: $p is what the snapshot had ($(tr -cd '\0' <"$work/want" | wc -c) entries)"
  done
  rm -rf "$work"
  return "$bad"
}

case $cmd in
  __in-snapshot)
    snap=$1
    shift
    mounts
    mkdir -p "$ns"
    mount -t tmpfs -o mode=700 "$NAME" "$ns"
    mkdir "$ns/state" "$ns/tmp" "$ns/drill"
    touch "$ns/password"
    mount --bind "$state" "$ns/state"
    mount --bind "${TMPDIR:-/tmp}" "$ns/tmp"
    mount --bind "$PASSWORD" "$ns/password"
    if [ "${1:-}" = --bind ]; then
      mount --bind "$2" "$ns/drill"
      shift 2
    fi
    export TMPDIR=$ns/tmp RESTIC_PASSWORD_FILE=$ns/password RESTIC_CACHE_DIR=$ns/state/cache
    [ -z "${RCLONE_CONFIG:-}" ] || export RCLONE_CONFIG=$ns/state/rclone.conf
    i=0
    for ds in "${order[@]}"; do
      i=$((i + 1))
      # ZFS won't mount on a directory that isn't empty: on an empty one
      # first, then over the dataset's path.
      mkdir "$ns/$i"
      mount -t zfs -o ro "$ds@$snap" "$ns/$i"
      mount --bind "$ns/$i" "${MP[$ds]}"
    done
    exec "$@"
    ;;
  __compare)
    compare "$@"
    exit
    ;;
esac

# Hand a failure of the automatic run to a Claude job in the machine's own
# project, once while one is still on it (or was in the last 6 hours). The
# owner hears only once the newest copy off the box is STALE_AFTER hours old
# (at once, with no proj). Run by hand, a command only says what failed.
failed() {
  trap - ERR
  set +e
  local why=$1 f="$state/job" now last id
  echo "$why" >&2
  [ "$cmd" = run ] || exit 1
  now=$(date +%s)
  [ -e "$state/failed-since" ] || echo "$now" >"$state/failed-since"
  if [ -n "$PROJ" ] && { [ ! -s "$f" ] || [ $((now - $(stat -c %Y "$f"))) -ge 21600 ]; }; then
    if id=$(runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" XDG_RUNTIME_DIR="/run/user/$USER_UID" \
        "$PROJ" run "$PROJECT" "$NAME, the off-site backup, failed on $HOST: $why

The journal has the rest (journalctl -u $NAME). Find the cause and fix it for good: in this flake if it's the machine's config, upstream in burrow (modules/backup.nix) if it's the module. \`$NAME status\` shows where it stands and \`sudo systemctl start $NAME\` runs it again. Never forget, prune or delete anything in the repository by hand, and never remove or re-create it: it's the only copy off the box. If it needs $OWNER (a login only they can renew, say), say exactly what."); then
      echo "$id" >"$f"
      echo "handed to Claude: proj log $id"
    fi
  fi
  last=$(jq -r .epoch "$state/last.json" 2>/dev/null || echo 0)
  if [ -z "$PROJ" ] || [ $((now - last)) -ge $((STALE_AFTER * 3600)) ]; then
    "$NOTIFY" open backup "$HOST's off-site backup is behind" \
      "The newest copy off $HOST is from $(if [ "$last" = 0 ]; then echo never; else date -d "@$last" -Is; fi). The last run said: $why

\`$NAME status\` on $HOST shows more.${PROJ:+ A Claude job has it (\`proj status\`).}"
  fi
  exit 1
}

# In the automatic run, whatever fails unforeseen is reported like the rest,
# once, from this shell: a subshell only passes its failure up.
if [ "$cmd" = run ]; then
  set -E
  trap 'rc=$?; [ "$BASHPID" = "$$" ] || exit "$rc"; failed "$BASH_COMMAND exited $rc"' ERR
fi

mkdir -p "$state"
chmod 700 "$state"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# One at a time: a backup, a drill, a restore by hand. Each of them may
# refresh rclone's token.
exec 9>"/run/$NAME.lock"
if [ "$cmd" = status ]; then
  if [ -e "$state/last.json" ]; then
    jq -r '"last backup \(.time): \(.snapshot), \(.files) files, \(.bytes / 1e9 * 100 | round / 100) GB read, \(.added / 1e6 * 100 | round / 100) MB sent"' "$state/last.json"
  else
    echo "no backup yet"
  fi
  [ ! -e "$state/failed-since" ] || echo "failing since $(date -d "@$(cat "$state/failed-since")" -Is)"
  [ ! -e "$state/maintained" ] || echo "pruned, checked and drilled $(date -r "$state/maintained" -Is)"
  flock -n 9 || { echo "a backup or drill is running (journalctl -u $NAME)"; exit 0; }
else
  flock -n 9 || { echo "waiting for the backup that's running" >&2; flock 9; }
fi

export RESTIC_REPOSITORY=$REPO
export RESTIC_PASSWORD_FILE=$PASSWORD
export RESTIC_CACHE_DIR=$state/cache
export RESTIC_PACK_SIZE=$PACK_SIZE
export RESTIC_COMPRESSION=auto
# Each mount of a snapshot is a device of its own: without this, restic would
# write every directory's tree again on every run.
export RESTIC_FEATURES=device-id-for-hardlinks
# rclone writes refreshed tokens back to its config, and some remotes' refresh
# tokens work only once: a copy of its own that keeps them, started over only
# when the secret it came from changes.
if [ -n "$RCLONE_CONFIG_FILE" ]; then
  [ -r "$RCLONE_CONFIG_FILE" ] || failed "can't read rclone's config at $RCLONE_CONFIG_FILE"
  seed=$(sha256sum <"$RCLONE_CONFIG_FILE")
  if [ ! -s "$state/rclone.conf" ] || [ "$(cat "$state/rclone.seed" 2>/dev/null)" != "$seed" ]; then
    install -m 600 "$RCLONE_CONFIG_FILE" "$state/rclone.conf"
    echo "$seed" >"$state/rclone.seed"
  fi
  export RCLONE_CONFIG=$state/rclone.conf
fi

# The same rules for every backup, the drill's included.
backup_flags=(--one-file-system --exclude-caches --exclude-if-present .nobackup --exclude-file "$EXCLUDE_FILE")

# The newest snapshot matching PATTERN that every dataset has, as
# NAME<TAB>CREATION (epoch seconds); nothing if there's none.
newest_snapshot() {
  local name creation ds
  while IFS=$'\t' read -r name creation; do
    name=${name#*@}
    # shellcheck disable=SC2053 # PATTERN is a glob on purpose
    [[ $name == $PATTERN ]] || continue
    for ds in "${DATASETS[@]:1}"; do
      zfs list -H -o name "$ds@$name" >/dev/null 2>&1 || continue 2
    done
    printf '%s\t%s\n' "$name" "$creation"
    return
  done < <(zfs list -H -p -t snapshot -o name,creation -S creation -d 1 "${DATASETS[0]}")
}

# The restic snapshot of the newest backup, as JSON.
latest() {
  restic snapshots --json --latest 1 --host "$HOST" | jq -e 'sort_by(.time) | last'
}

# Let go of the backup's holds on every snapshot of the datasets but KEEP (the
# newest one off the box, if any): ones from runs that died stay held no
# longer than the next run.
release_except() {
  local keep=$1 ds s
  for ds in "${DATASETS[@]}"; do
    while IFS= read -r s; do
      if [ "$s" != "$ds@$keep" ] && zfs holds -H "$s" | cut -f2 | grep -qxF "$hold"; then
        zfs release "$hold" "$s"
      fi
    done < <(zfs list -H -t snapshot -o name,userrefs -d 1 "$ds" | awk -F '\t' '$2 > 0 { print $1 }')
  done
}

# Everything the snapshot NAME has (at PATHs, the datasets), backup ID has, as
# the backup's own rules leave it: a fresh backup of the same snapshot lists
# the same entries, of the same kind, mode, owner, size, times and inode. Not
# the same device: each mount of a snapshot gets its own, and restic keeps it
# for hardlinked files. The fresh backup is forgotten after; DIR holds the
# listings.
holds_all() {
  local dir=$1 id=$2 name=$3 old=() check ok=0
  shift 3
  # One a drill that died left behind.
  mapfile -t old < <(restic snapshots --json --host "$HOST-drill" | jq -r '.[].id')
  [ "${#old[@]}" = 0 ] || restic forget -q "${old[@]}" >/dev/null || true
  if ! in_snapshot "$name" restic backup --json --quiet --host "$HOST-drill" --tag drill --parent "$id" \
      "${backup_flags[@]}" "$@" >"$dir/check.json" 2>"$dir/check.err"; then
    echo "drill: a fresh backup of $name failed:"; tail -5 "$dir/check.err"; return 1
  fi
  check=$(jq -r 'select(.message_type == "summary") | .snapshot_id' "$dir/check.json")
  [ -n "$check" ] || { echo "drill: a fresh backup of $name made no snapshot"; return 1; }
  { restic ls --json "$id" | under fields "$@" | LC_ALL=C sort -T "$dir" >"$dir/held" &&
    restic ls --json "$check" | under fields "$@" | LC_ALL=C sort -T "$dir" >"$dir/fresh"; } || ok=1
  restic forget -q "$check" >/dev/null || true
  [ "$ok" = 0 ] || { echo "drill: couldn't list backup ${id:0:8}"; return 1; }
  if ! cmp -s "$dir/held" "$dir/fresh"; then
    echo "drill: backup ${id:0:8} doesn't hold all of $name; where a fresh backup of it differs (< the backup, > fresh):"
    { LC_ALL=C comm -23 "$dir/held" "$dir/fresh" | jq -r '"< " + .[0]'
      LC_ALL=C comm -13 "$dir/held" "$dir/fresh" | jq -r '"> " + .[0]'; } | head -20
    return 1
  fi
  echo "drill: backup ${id:0:8} holds all of $name"
}

# Restore PATHs from backup ID to DIR/tree and compare them with the snapshot
# NAME they were read from.
restores() {
  local dir=$1 id=$2 name=$3 p inc includes=()
  shift 3
  for p in "$@"; do
    inc=${p//\\/\\\\}; inc=${inc//\*/\\*}; inc=${inc//\?/\\?}; inc=${inc//\[/\\[}
    includes+=(--include "$inc")
  done
  if ! restic ls --json "$id" | under names "$@" >"$dir/expected"; then
    echo "drill: couldn't list backup ${id:0:8}"; return 1
  fi
  if ! restic restore "$id" --target "$dir/tree" "${includes[@]}" -q; then
    echo "drill: restic restore ${id:0:8} failed"; return 1
  fi
  in_snapshot "$name" --bind "$dir" "$self" __compare "$ns/drill" "$@"
}

# Check the newest backup against the snapshot it was read from (held until
# the next one, so it's there), then restore drill_paths from it and compare.
drill() {
  local snap id name ds paths=() dir rc=0
  snap=$(latest) || { echo "drill: no backup to restore"; return 1; }
  id=$(jq -r .id <<<"$snap")
  name=$(jq -r '.tags[0]' <<<"$snap")
  for ds in "${DATASETS[@]}"; do
    zfs list -H -o name "$ds@$name" >/dev/null 2>&1 || { echo "drill: $ds@$name is gone, nothing to check backup ${id:0:8} against"; return 1; }
  done
  mounts || return 1
  for ds in "${order[@]}"; do paths+=("${MP[$ds]}"); done
  mkdir -p "$SCRATCH" || return 1
  dir=$(mktemp -d "$SCRATCH/$NAME-drill.XXXXXX") || return 1
  # A cache to restic, so no backup reads it should a snapshot catch it.
  printf 'Signature: 8a477f597d28d172789f06886806bc55\n' >"$dir/CACHEDIR.TAG"
  if ! holds_all "$dir" "$id" "$name" "${paths[@]}"; then
    rc=1
  elif [ "${#drill_paths[@]}" -gt 0 ]; then
    restores "$dir" "$id" "$name" "${drill_paths[@]}" || rc=1
  fi
  rm -rf "$dir"
  return "$rc"
}

drill_paths=("${DRILL[@]}")
case $cmd in
  restic)
    exec restic "$@"
    ;;
  status)
    restic snapshots --compact --host "$HOST" --latest 3 2>/dev/null || echo "repository unreachable"
    restic stats --mode raw-data 2>/dev/null | grep -E 'Total (Uncompressed )?Size|Compression Ratio' || true
    exit 0
    ;;
  drill)
    [ "$#" = 0 ] || drill_paths=("$@")
    drill
    exit
    ;;
  run) ;;
  *) echo "usage: $NAME [status | drill [PATH...] | restic ARGS...]" >&2; exit 2 ;;
esac

# Once a day, after a backup: forget what KEEP no longer keeps and prune it,
# read back the day's share of the repository (all of it every VERIFY_DAYS
# days), then the drill.
maintain() {
  if [ -e "$state/maintained" ] && [ $(($(date +%s) - $(stat -c %Y "$state/maintained"))) -lt 72000 ]; then
    return 0
  fi
  restic forget --host "$HOST" "${KEEP[@]}" --prune -q || failed "restic forget --prune failed"
  local part=$((10#$(date +%j) % VERIFY_DAYS + 1))
  restic check -q --read-data-subset="$part/$VERIFY_DAYS" \
    || failed "restic check found damage reading part $part/$VERIFY_DAYS of the repository back"
  drill || failed "the restore drill failed"
  touch "$state/maintained"
}

last=$(cat "$state/last" 2>/dev/null || true)
release_except "$last"

snap=
IFS=$'\t' read -r snap created < <(newest_snapshot) || true
[ -n "$snap" ] || failed "no snapshot of ${DATASETS[0]} matches $PATTERN"
if [ $(($(date +%s) - created)) -ge $((STALE_AFTER * 3600)) ]; then
  failed "the newest snapshot matching $PATTERN, $snap, is more than $STALE_AFTER hours old: is whatever takes them still running?"
fi

restic unlock -q 2>/dev/null || true
if [ "$last" != "$snap" ]; then
  rc=0
  restic cat config >/dev/null 2>"$tmp/err" || rc=$?
  case $rc in
    0) ;;
    10) restic init -q || failed "couldn't create the repository at $REPO" ;;
    *) failed "couldn't open the repository at $REPO (restic exit $rc): $(tail -3 "$tmp/err")" ;;
  esac

  # Held while it's read, and after, for the drill.
  for ds in "${DATASETS[@]}"; do zfs hold "$hold" "$ds@$snap" 2>/dev/null || true; done
  if ! mounts 2>"$tmp/err"; then
    release_except "$last"
    failed "$(cat "$tmp/err")"
  fi
  paths=()
  for ds in "${order[@]}"; do paths+=("${MP[$ds]}"); done
  rc=0
  in_snapshot "$snap" restic backup --json --quiet --host "$HOST" --tag "$snap" --time "$(date -d "@$created" '+%F %T')" \
    "${backup_flags[@]}" "${paths[@]}" >"$tmp/out" 2>"$tmp/err" || rc=$?
  summary=$(jq -c 'select(.message_type == "summary")' "$tmp/out" 2>/dev/null | tail -1 || true)
  if [ "$rc" != 0 ] || [ -z "$summary" ]; then
    release_except "$last"
    failed "restic backup of $snap exited $rc: $( (jq -r 'select(.message_type == "error") | "\(.item // ""): \(.error.message // .error)"' "$tmp/err" 2>/dev/null || tail -5 "$tmp/err") | head -5 || true)"
  fi
  jq -n --argjson s "$summary" --arg snap "$snap" --arg t "$(date -Is)" --argjson e "$(date +%s)" \
    '{time: $t, epoch: $e, snapshot: $snap, id: $s.snapshot_id, files: $s.total_files_processed,
      bytes: $s.total_bytes_processed, added: $s.data_added_packed}' >"$state/last.json"
  echo "$snap" >"$state/last"
  release_except "$snap"
  jq -r '"backed up \(.snapshot): \(.files) files, \(.bytes / 1e9 * 100 | round / 100) GB read, \(.added / 1e6 * 100 | round / 100) MB sent"' "$state/last.json"
fi

maintain
if [ -e "$state/failed-since" ]; then
  rm -f "$state/failed-since" "$state/job"
  "$NOTIFY" close backup "Backed up $snap."
fi
