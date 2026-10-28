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
#                          restore PATHs (default DRILL) from the newest backup
#                          to scratch and compare them with the snapshot they
#                          came from, byte for byte
#   HOST-backup restic ARGS...
#                          restic itself, on this repository (to restore by
#                          hand: restic restore latest --target DIR --include PATH)
#
# Restic encrypts everything before it leaves, with the password in PASSWORD:
# without it the repository is noise, so its owner keeps a copy off the box.
# (Everything in capitals comes from ../backup.nix.)

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

state=$DIR
read -r -a datasets <<<"$DATASETS"
read -r -a drill_paths <<<"$DRILL"
hold=burrow-backup
mkdir -p "$state"
chmod 700 "$state"

export RESTIC_REPOSITORY=$REPO
export RESTIC_PASSWORD_FILE=$PASSWORD
export RESTIC_CACHE_DIR=$state/cache
export RESTIC_PACK_SIZE=$PACK_SIZE
export RESTIC_COMPRESSION=auto
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# rclone writes refreshed tokens back to its config: a copy of its own, so the
# secret it came from stays as it is.
if [ -n "$RCLONE_CONFIG_FILE" ]; then
  install -m 600 "$RCLONE_CONFIG_FILE" "$tmp/rclone.conf"
  export RCLONE_CONFIG=$tmp/rclone.conf
fi

# Where a dataset is mounted (its own root, not a bind mount out of it).
mountpoint_of() {
  findmnt -rn -o TARGET,FSROOT -S "$1" | awk '$2 == "/" { print $1; exit }'
}

# The newest snapshot matching PATTERN that every dataset has, as
# NAME<TAB>CREATION (epoch seconds); nothing if there's none.
newest_snapshot() {
  local name creation ds
  while IFS=$'\t' read -r name creation; do
    name=${name#*@}
    # shellcheck disable=SC2053 # PATTERN is a glob on purpose
    [[ $name == $PATTERN ]] || continue
    for ds in "${datasets[@]:1}"; do
      zfs list -H -o name "$ds@$name" >/dev/null 2>&1 || continue 2
    done
    printf '%s\t%s\n' "$name" "$creation"
    return
  done < <(zfs list -H -p -t snapshot -o name,creation -S creation -d 1 "${datasets[0]}")
}

# The restic snapshot of the newest backup, as JSON.
latest() {
  restic snapshots --json --latest 1 --host "$HOST" | jq -e 'sort_by(.time) | last'
}

case "${1:-run}" in
  restic)
    shift
    exec restic "$@"
    ;;
  status)
    if [ -e "$state/last.json" ]; then
      jq -r '"last backup \(.time): \(.snapshot), \(.files) files, \(.bytes / 1e9 * 100 | round / 100) GB read, \(.added / 1e6 * 100 | round / 100) MB sent"' "$state/last.json"
    else
      echo "no backup yet"
    fi
    [ ! -e "$state/failed-since" ] || echo "failing since $(date -d "@$(cat "$state/failed-since")" -Is)"
    [ ! -e "$state/maintained" ] || echo "pruned, checked and drilled $(date -r "$state/maintained" -Is)"
    restic snapshots --compact --host "$HOST" --latest 3 2>/dev/null || echo "repository unreachable"
    restic stats --mode raw-data 2>/dev/null | grep -E 'Total (Uncompressed )?Size|Compression Ratio' || true
    exit 0
    ;;
  drill)
    shift
    [ "$#" = 0 ] || drill_paths=("$@")
    ;;
  run) ;;
  *) echo "usage: $NAME [status | drill [PATH...] | restic ARGS...]" >&2; exit 2 ;;
esac

exec 9>"/run/$NAME.lock"
flock 9

# Kind, mode, owner and size of each of $tmp/files under $1, and the mtime of
# the regular files among them.
meta() {
  (cd "$1" && xargs -0r -a "$tmp/files" bash -c \
    'exec find -P "$@" -maxdepth 0 \( -type f -printf "%p %y %m %U %G %s %T@\n" -o -printf "%p %y %m %U %G\n" \)' find-meta) | LC_ALL=C sort
}

# Restore drill_paths from the newest backup into scratch and compare them
# with the snapshot that backup was read from (held until the next one, so it's
# there). Everything the backup has must be what the snapshot had; what the
# snapshot has beyond it can only be what the backup leaves out (EXCLUDE_FILE,
# caches, .nobackup).
drill() {
  local snap id name restored p ds m mnt src out ok=1 args=()
  snap=$(latest) || { echo "drill: no backup to restore"; return 1; }
  id=$(jq -r .short_id <<<"$snap")
  name=$(jq -r '.tags[0]' <<<"$snap")
  mkdir -p "$SCRATCH"
  restored=$(mktemp -d "$SCRATCH/$NAME-drill.XXXXXX")
  # A cache to restic, so no backup reads it should a snapshot catch it.
  printf 'Signature: 8a477f597d28d172789f06886806bc55\n' >"$restored/CACHEDIR.TAG"
  for p in "${drill_paths[@]}"; do args+=(--include "$p"); done
  restic restore "$id" --target "$restored" "${args[@]}" -q || { echo "drill: restic restore $id failed"; ok=; }
  for p in "${drill_paths[@]}"; do
    [ -n "$ok" ] || break
    mnt=
    for ds in "${datasets[@]}"; do
      m=$(mountpoint_of "$ds")
      case ${p%/}/ in "${m%/}"/*) [ "${#m}" -le "${#mnt}" ] || mnt=$m ;; esac
    done
    [ -n "$mnt" ] || { echo "drill: $p is on none of $DATASETS"; ok=; break; }
    src=${mnt%/}/.zfs/snapshot/$name${p#"${mnt%/}"}
    [ -e "$restored$p" ] || { echo "drill: $p isn't in backup $id"; ok=; break; }
    [ -e "$src" ] || { echo "drill: $src is gone, nothing to compare $p with"; ok=; break; }
    out=$(LC_ALL=C diff -r --no-dereference "$src" "$restored$p" 2>&1 | grep -v "^Only in $src" || true)
    if [ -n "$out" ]; then
      echo "drill: $p in backup $id isn't what $name had:"; head -20 <<<"$out"; ok=; break
    fi
    (cd "$restored$p" && find . -mindepth 1 ! -type d -print0) >"$tmp/files"
    if ! diff <(meta "$src") <(meta "$restored$p") >"$tmp/meta"; then
      echo "drill: the metadata of $p in backup $id isn't what $name had:"; head -20 "$tmp/meta"; ok=; break
    fi
    echo "drill: $p from backup $id is what $name had ($(tr -cd '\0' <"$tmp/files" | wc -c) files)"
  done
  rm -rf "$restored"
  [ -n "$ok" ]
}

if [ "${1:-run}" = drill ]; then
  drill
  exit
fi

# Hand a failure to a Claude job in the machine's own project, once while one
# is still on it (or was in the last 6 hours). The owner hears only once the
# newest copy off the box is STALE_AFTER hours old (at once, with no proj).
failed() {
  local why=$1 f="$state/job" now last id
  now=$(date +%s)
  echo "$why" >&2
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

# Once a day, after a backup: forget what KEEP no longer keeps and prune it,
# read back the day's share of the repository (all of it every VERIFY_DAYS
# days), then the drill.
maintain() {
  if [ -e "$state/maintained" ] && [ $(($(date +%s) - $(stat -c %Y "$state/maintained"))) -lt 72000 ]; then
    return 0
  fi
  # shellcheck disable=SC2086 # KEEP is a list of flags
  restic forget --host "$HOST" $KEEP --prune -q || failed "restic forget --prune failed"
  local part=$((10#$(date +%j) % VERIFY_DAYS + 1))
  restic check -q --read-data-subset="$part/$VERIFY_DAYS" \
    || failed "restic check found damage reading part $part/$VERIFY_DAYS of the repository back"
  if [ "${#drill_paths[@]}" -gt 0 ]; then
    drill || failed "the restore drill failed"
  fi
  touch "$state/maintained"
}

snap=
IFS=$'\t' read -r snap created < <(newest_snapshot) || true
[ -n "$snap" ] || failed "no snapshot of ${datasets[0]} matches $PATTERN"
if [ $(($(date +%s) - created)) -ge $((STALE_AFTER * 3600)) ]; then
  failed "the newest snapshot matching $PATTERN, $snap, is more than $STALE_AFTER hours old: is whatever takes them still running?"
fi

restic unlock -q 2>/dev/null || true
if [ "$(cat "$state/last" 2>/dev/null)" != "$snap" ]; then
  rc=0
  restic cat config >/dev/null 2>"$tmp/err" || rc=$?
  case $rc in
    0) ;;
    10) restic init -q || failed "couldn't create the repository at $REPO" ;;
    *) failed "couldn't open the repository at $REPO (restic exit $rc): $(tail -3 "$tmp/err")" ;;
  esac

  # Hold the snapshot while it's read, and after, for the drill.
  prev=$(cat "$state/last" 2>/dev/null || true)
  for ds in "${datasets[@]}"; do zfs hold "$hold" "$ds@$snap" 2>/dev/null || true; done
  release() {
    for ds in "${datasets[@]}"; do zfs release "$hold" "$ds@$1" 2>/dev/null || true; done
  }

  # Each dataset's snapshot mounted over its own path, in a mount namespace of
  # the backup's own: restic sees the paths it saw last time, so it reads only
  # the files that changed since.
  paths=()
  mounts=
  for ds in "${datasets[@]}"; do
    m=$(mountpoint_of "$ds")
    [ -n "$m" ] || { release "$snap"; failed "$ds isn't mounted"; }
    paths+=("$m")
    mounts+="mount -t zfs -o ro $(printf %q "$ds@$snap") $(printf %q "$m") && "
  done
  rc=0
  unshare --mount --propagation private -- bash -c "${mounts}exec \"\$@\"" backup \
    restic backup --json --quiet --host "$HOST" --tag "$snap" --time "$(date -d "@$created" '+%F %T')" \
      --one-file-system --exclude-caches --exclude-if-present .nobackup --exclude-file "$EXCLUDE_FILE" \
      "${paths[@]}" >"$tmp/out" 2>"$tmp/err" || rc=$?
  summary=$(jq -c 'select(.message_type == "summary")' "$tmp/out" 2>/dev/null | tail -1)
  if [ "$rc" != 0 ] || [ -z "$summary" ]; then
    release "$snap"
    failed "restic backup of $snap exited $rc: $( (jq -r 'select(.message_type == "error") | "\(.item // ""): \(.error.message // .error)"' "$tmp/err" 2>/dev/null || tail -5 "$tmp/err") | head -5)"
  fi
  jq -n --argjson s "$summary" --arg snap "$snap" --arg t "$(date -Is)" --argjson e "$(date +%s)" \
    '{time: $t, epoch: $e, snapshot: $snap, id: $s.snapshot_id, files: $s.total_files_processed,
      bytes: $s.total_bytes_processed, added: $s.data_added_packed}' >"$state/last.json"
  echo "$snap" >"$state/last"
  if [ -n "$prev" ] && [ "$prev" != "$snap" ]; then release "$prev"; fi
  jq -r '"backed up \(.snapshot): \(.files) files, \(.bytes / 1e9 * 100 | round / 100) GB read, \(.added / 1e6 * 100 | round / 100) MB sent"' "$state/last.json"
fi

maintain
if [ -e "$state/failed-since" ]; then
  rm -f "$state/failed-since" "$state/job"
  "$NOTIFY" close backup "Backed up $snap."
fi
