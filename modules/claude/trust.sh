# claude-trust DIR...: marks each DIR trusted in the user's Claude Code config
# (~/.claude.json), the answer to the question Claude Code asks on its first
# start in a folder, for the Claudes that start in one with nobody at their
# terminal to answer it: the Remote Control servers (remote-control/serve.sh)
# and the usage watcher's Claude Code (usage-reset.nix). Claude keys trust by
# the folder's real path; the spelling given is seeded too. A folder that
# isn't there yet is trusted at the path it will have.

[ $# -gt 0 ] || { echo "usage: claude-trust DIR..." >&2; exit 2; }
cfg=$HOME/.claude.json

paths=()
for dir in "$@"; do
  paths+=("$dir")
  real=$(realpath -m "$dir")
  [ "$real" = "$dir" ] || paths+=("$real")
done

trusted() {
  jq -e --args '[$ARGS.positional[] as $p | .projects[$p].hasTrustDialogAccepted == true] | all' \
    "${paths[@]}" <"$cfg" >/dev/null
}

[ -s "$cfg" ] || { echo "no $cfg yet: log in to Claude Code first (claude /login)" >&2; exit 1; }
trusted && exit 0

# The lock Claude Code itself takes on its config (a .lock directory), so this
# never interleaves with a running session's write. One left behind by a crash
# goes stale after Claude's own 10 seconds; give it 30.
lock=$cfg.lock
got=
for _ in $(seq 100); do
  if mkdir "$lock" 2>/dev/null; then got=1; break; fi
  if [ -n "$(find "$lock" -maxdepth 0 -mmin +0.5 2>/dev/null)" ]; then rmdir "$lock" 2>/dev/null || true; fi
  sleep 0.1
done
[ -n "$got" ] || { echo "$cfg stayed locked; trying again on the next start" >&2; exit 1; }
trap 'rmdir "$lock" 2>/dev/null' EXIT
tmp=$(mktemp "$cfg.XXXXXX")
jq --args 'reduce $ARGS.positional[] as $p (.; .projects[$p].hasTrustDialogAccepted = true)' \
  "${paths[@]}" <"$cfg" >"$tmp"
chmod 600 "$tmp"
mv -f "$tmp" "$cfg"
rmdir "$lock"
trap - EXIT
trusted
