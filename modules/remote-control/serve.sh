# claude-rc-serve NAME: the Remote Control server for PROJECTS/NAME, run by
# claude-rc@NAME inside its tmux. It trusts the folder (claude-trust) before
# Claude can ask about it (nobody is at that terminal to answer), then picks the
# spawn mode: sessions started from claude.ai each get their own git worktree,
# so two of them in one project never share a checkout, in the permission mode
# MODE (none: Claude's own default), with the Claude Code at CLAUDE. Both are
# baked into this script, so changing either makes a new one, and
# claude-rc-scan restarts the servers still on the old one. With IDLE_SESSION=0
# it starts no session of its own: each one is a full Claude holding a few GB,
# so a server only holds them while someone is using them. PROJECTS, FLAKE,
# SPAWN, SAME_DIR, MODE, CLAUDE and IDLE_SESSION come from ../remote-control.nix.

name=${1:?usage: claude-rc-serve NAME}
dir=$PROJECTS/$name
claude-trust "$dir"
real=$(realpath "$dir")

spawn=$SPAWN
# The machine's own flake stays one checkout: that's the one the switch builds.
# A folder that isn't a git repository can't have worktrees.
if [ "$real" = "$(realpath "$FLAKE")" ] || [ ! -d "$dir/.git" ]; then spawn=same-dir; fi
for n in "${SAME_DIR[@]}"; do
  if [ "$n" = "$name" ]; then spawn=same-dir; fi
done

idle=--no-create-session-in-dir
[ "$IDLE_SESSION" = 0 ] || idle=--create-session-in-dir
exec "$CLAUDE" remote-control --name "$name" --spawn="$spawn" "$idle" ${MODE:+--permission-mode "$MODE"}
