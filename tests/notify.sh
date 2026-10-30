# HOST-notify (modules/notify/notify.sh) with a stand-in for gh, which keeps
# GitHub's issues in a file and can be down, fail some of its commands, or
# lose its answer to an issue it opened: one issue a notice, a comment at
# most a day, closed when cleared, never a second one while it can't say
# which are open, and `pending` true until GitHub has shown it closed.
#
#   bash tests/notify.sh modules/notify/notify.sh
set -euo pipefail
notify_sh=$(realpath "$1")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/notices"
github=$tmp/github.json calls=$tmp/gh-calls journal=$tmp/journal
export GITHUB=$github CALLS=$calls

bash=$(command -v bash)
{ echo "#!$bash"; cat; } >"$tmp/bin/gh" <<'EOF'
# gh, as notify.sh calls it, on the issues in $GITHUB.
set -euo pipefail
echo "$*" >>"$CALLS"
gh=$(cat "$GITHUB")
if jq -e --arg c "$1" '.down or (.fail | index($c))' <<<"$gh" >/dev/null; then
  echo "gh: couldn't reach GitHub" >&2
  exit 1
fi
opt() { local want=$1; shift; while [ $# -gt 1 ]; do [ "$1" = "$want" ] && { echo "$2"; return; }; shift; done; }
save() { jq "$@" <<<"$gh" >"$GITHUB"; }
case "$1 ${2:-}" in
  "api -X")
    [[ " $* " == *" repos/sam/homelab/issues "* && " $* " == *" labels=updates "* && " $* " == *" state=open "* ]] ||
      { echo "gh: unexpected listing: $*" >&2; exit 1; }
    jq '[.issues[] | select(.open) | {number, title, pull_request: null}]' <<<"$gh" | jq -r "$(opt --jq "$@")"
    ;;
  "label create") ;;
  "issue create")
    save --arg t "$(opt --title "$@")" '.issues += [{number: (.issues | length + 1), title: $t, open: true, comments: 0}]'
    if jq -e .lose <<<"$gh" >/dev/null; then
      gh=$(cat "$GITHUB") save '.lose = false'
      echo "gh: connection reset" >&2
      exit 1
    fi
    ;;
  "issue comment") save --argjson n "$3" '.issues[$n - 1].comments += 1' ;;
  "issue close") save --argjson n "$3" '.issues[$n - 1].open = false' ;;
  *) echo "gh: unexpected $*" >&2; exit 1 ;;
esac
EOF
printf '#!%s\ncat >>%q\n' "$bash" "$journal" >"$tmp/bin/systemd-cat"
chmod +x "$tmp/bin/"*

notify() {
  env NOTIFY=box-notify REPO="${REPO-sam/homelab}" ASSIGNEE= LABEL=updates DIR="$tmp/notices" \
    BURROW_USER="$(id -un)" HOME_DIR="$tmp" PATH="$tmp/bin:$PATH" bash -euo pipefail "$notify_sh" "$@"
}
# GitHub as gh finds it: up or down (and which commands fail), whether it
# loses its answer to the next issue it opens, and its issues.
github() {
  jq -n --argjson down "${DOWN:-false}" --argjson fail "${FAIL:-[]}" --argjson lose "${LOSE:-false}" \
    --argjson issues "${1:-[]}" '{down: $down, fail: $fail, lose: $lose, issues: $issues}' >"$github"
}
issues() { jq -c "[.issues[] | $1]" "$github"; }
open_titles() { issues 'select(.open) | .title'; }
pending() { if notify pending "$1"; then echo yes; else echo no; fi; }
fail() { echo "FAIL: $*" >&2; exit 1; }
want() { [ "$2" = "$3" ] || fail "$1: wanted $3, got $2"; }
fresh() { rm -f "$tmp/notices/"* "$calls" "$journal"; github; }

# One issue a notice, with its key in the title; again the same day is
# nothing new, a day later one comment.
fresh
notify open k1 "Stuck" "It's stuck."
want "opened" "$(open_titles)" '["[k1] Stuck"]'
want "pending once open" "$(pending k1)" yes
notify open k1 "Stuck" "It's stuck."
want "opened again" "$(issues '.comments')" '[0]'
echo $(($(date +%s) - 90000)) >"$tmp/notices/k1"
notify open k1 "Stuck" "It's stuck."
want "a day later" "$(issues '.comments')" '[1]'
want "still one" "$(open_titles)" '["[k1] Stuck"]'

# Cleared: closed, and no longer pending; a close for nothing it opened asks
# GitHub nothing, and nor does pending.
notify close k1 "Fixed."
want "closed" "$(open_titles)" '[]'
want "pending once closed" "$(pending k1)" no
rm -f "$calls"
notify close k1 "Fixed."
notify pending k1 || true
[ ! -e "$calls" ] || fail "asked GitHub: $(cat "$calls")"

# GitHub down: nothing opened and nothing pending, said in the journal, and
# notify itself never fails. A close while it's down is still pending, and
# goes through once it's back.
DOWN=true fresh
notify open k2 "Stuck" "It's stuck."
want "opened while down" "$(issues .number)" '[]'
want "pending while down" "$(pending k2)" no
grep -q "box-notify open k2: couldn't reach GitHub" "$journal" || fail "journal: $(cat "$journal")"
fresh
notify open k2 "Stuck" "It's stuck."
DOWN=true github "$(jq -c .issues "$github")"
notify close k2 "Fixed."
want "closed while down" "$(open_titles)" '["[k2] Stuck"]'
want "pending after a close that didn't get through" "$(pending k2)" yes
github "$(jq -c .issues "$github")"
notify close k2 "Fixed."
want "closed once back" "$(open_titles)" '[]'
want "pending once back" "$(pending k2)" no

# An issue opened whose answer was lost is still pending, and closes.
LOSE=true fresh
notify open k3 "Stuck" "It's stuck."
want "opened, answer lost" "$(open_titles)" '["[k3] Stuck"]'
want "pending, answer lost" "$(pending k3)" yes
notify close k3 "Fixed."
want "closed, answer lost" "$(open_titles)" '[]'

# A close GitHub refuses is still pending.
fresh
notify open k6 "Stuck" "It's stuck."
FAIL='["issue"]' github "$(jq -c .issues "$github")"
notify close k6 "Fixed."
want "closed though refused" "$(open_titles)" '["[k6] Stuck"]'
want "pending after a refused close" "$(pending k6)" yes

# While it can't list the open issues, none is opened twice; a twin left
# from before closes with the rest.
fresh
notify open k4 "Stuck" "It's stuck."
FAIL='["api"]' github "$(jq -c .issues "$github")"
notify open k4 "Stuck" "It's stuck."
want "opened twice" "$(issues .number)" '[1]'
github "$(jq -c '.issues + [.issues[0] | .number = 2]' "$github")"
notify close k4 "Fixed."
want "twins closed" "$(open_titles)" '[]'

# No repo: the journal only.
REPO='' fresh
REPO='' notify open k5 "Stuck" "It's stuck."
[ ! -e "$calls" ] || fail "no repo, asked GitHub: $(cat "$calls")"
grep -q "box-notify open k5 Stuck" "$journal" || fail "journal: $(cat "$journal")"
echo "notify: ok"
