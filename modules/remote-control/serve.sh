# claude-rc-serve NAME: the Remote Control server for PROJECTS/NAME, run by
# claude-rc@NAME inside its tmux. It answers the folder's trust question before
# Claude can ask it (nobody is at that terminal to answer it), then picks the
# spawn mode: sessions started from claude.ai each get their own git worktree,
# so two of them in one project never share a checkout, in the permission mode
# MODE (none: Claude's own default). PROJECTS, FLAKE, SPAWN, SAME_DIR and MODE
# come from ../remote-control.nix.

name=${1:?usage: claude-rc-serve NAME}
dir=$PROJECTS/$name
cfg=$HOME/.claude.json

# Claude keys trust by the folder's real path; seed the spelling we were given too.
paths=("$dir")
real=$(realpath "$dir")
[ "$real" = "$dir" ] || paths+=("$real")

trusted() {
  jq -e --args '[$ARGS.positional[] as $p | .projects[$p].hasTrustDialogAccepted == true] | all' \
    "${paths[@]}" <"$cfg" >/dev/null
}

[ -s "$cfg" ] || { echo "no $cfg yet: log in to Claude Code first (claude /login)" >&2; exit 1; }
if ! trusted; then
  # The lock Claude Code itself takes on its config (a .lock directory), so this
  # never interleaves with a running session's write. One left behind by a
  # crash goes stale after Claude's own 10 seconds; give it 30.
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
fi

spawn=$SPAWN
# The machine's own flake stays one checkout: that's the one the switch builds.
# A folder that isn't a git repository can't have worktrees.
if [ "$real" = "$(realpath "$FLAKE")" ] || [ ! -d "$dir/.git" ]; then spawn=same-dir; fi
for n in "${SAME_DIR[@]}"; do
  if [ "$n" = "$name" ]; then spawn=same-dir; fi
done

exec claude remote-control --name "$name" --spawn="$spawn" ${MODE:+--permission-mode "$MODE"}
