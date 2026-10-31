# claude-trust (modules/claude/trust.sh) on a stand-in ~/.claude.json: each
# folder trusted under the spelling given and its real path, the rest of the
# config kept, a folder not made yet trusted at its path, nothing written
# when it's trusted already, and Claude Code's own lock on the config kept
# to: waited for, taken over once it's stale, never written through.
#
#   bash tests/claude-trust.sh "$(nix build --print-out-paths .#...claude-trust)/bin/claude-trust"
set -euo pipefail
trust=$1
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME=$tmp/home
cfg=$HOME/.claude.json
mkdir -p "$HOME" "$tmp/real/project"
ln -s "$tmp/real" "$tmp/link"

fail() { echo "FAIL: $*" >&2; exit 1; }
trusted() { jq -e --arg p "$1" '.projects[$p].hasTrustDialogAccepted == true' "$cfg" >/dev/null; }

# Not logged in: nothing to add to, so it says so.
if out=$("$trust" "$tmp/link/project" 2>&1); then fail "trusted with no config: $out"; fi
[[ $out == *"log in"* ]] || fail "no config: $out"
[ ! -e "$cfg" ] || fail "made a config of its own"

# Both spellings, the rest kept, the mode Claude Code gives it.
echo '{"projects":{"/elsewhere":{"allowedTools":["x"]}},"numStartups":3}' >"$cfg"
chmod 644 "$cfg"
"$trust" "$tmp/link/project" "$tmp/later/claude"
trusted "$tmp/link/project" || fail "the spelling given isn't trusted"
trusted "$tmp/real/project" || fail "the real path isn't trusted"
trusted "$tmp/later/claude" || fail "a folder not made yet isn't trusted"
jq -e '.numStartups == 3 and .projects["/elsewhere"].allowedTools == ["x"]' "$cfg" >/dev/null ||
  fail "lost the rest of the config: $(cat "$cfg")"
[ "$(stat -c %a "$cfg")" = 600 ] || fail "mode $(stat -c %a "$cfg")"
[ ! -e "$cfg.lock" ] || fail "left its lock"

# Trusted already: nothing written, so a held lock doesn't hold it up.
mkdir "$cfg.lock"
before=$(stat -c %i "$cfg")
"$trust" "$tmp/link/project"
[ "$(stat -c %i "$cfg")" = "$before" ] || fail "rewrote a config that had it already"

# A live writer's lock: waited on, then given up on, the config untouched.
if out=$("$trust" "$tmp/other" 2>&1); then fail "wrote through a held lock: $out"; fi
[[ $out == *"stayed locked"* ]] || fail "held lock: $out"
trusted "$tmp/other" && fail "wrote through a held lock"
[ -d "$cfg.lock" ] || fail "took a live writer's lock away"

# One a crash left behind: taken over.
touch -d '1 minute ago' "$cfg.lock"
"$trust" "$tmp/other"
trusted "$tmp/other" || fail "a stale lock kept it out"
[ ! -e "$cfg.lock" ] || fail "left the lock it took over"

echo "claude-trust: ok"
