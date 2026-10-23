# house-sync: keep every repo's copy of the house block (burrow's
# house/rules.md) the same as burrow's. A repo whose block differs gets one
# draft PR that rewrites only what's between the markers, from an ordinary
# branch, so the steward reviews and lands it like any other PR. (RULES,
# OWNERS, EXCLUDE, DRY_RUN and NOTIFY come from ../house.nix.)
#
#   house-sync          one pass
#   house-sync status   what a pass would do, changing nothing

set -euo pipefail
[ "${1:-}" = status ] && DRY_RUN=1

begin_re='^<!-- house:rules v[0-9]+ begin'
end_re='^<!-- house:rules end -->$'
rev=$(sha256sum "$RULES" | cut -c1-8)
branch="house/rules-$rev"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say() { echo "house-sync: $*"; }
read -ra globs <<<"$EXCLUDE"
excluded() {
  local r=$1 g
  for g in "${globs[@]}"; do
    # shellcheck disable=SC2053 # a glob, on purpose
    [[ $r == $g ]] && return 0
  done
  return 1
}

read -ra owners <<<"$OWNERS"
for owner in "${owners[@]}"; do
  gh repo list "$owner" --no-archived --source --limit 1000 \
    --json nameWithOwner,defaultBranchRef \
    --jq '.[] | select(.defaultBranchRef != null) | "\(.nameWithOwner) \(.defaultBranchRef.name)"'
done >"$tmp/repos"

# fd 3, so nothing in the loop can eat the list from stdin.
while read -r repo base <&3; do
  excluded "$repo" && continue
  f="$tmp/AGENTS.md"
  # No AGENTS.md yet: the baseline job writes it, block included.
  gh api "repos/$repo/contents/AGENTS.md?ref=$base" --jq .content 2>/dev/null | base64 -d >"$f" || continue

  begins=$(grep -cE "$begin_re" "$f" || true)
  ends=$(grep -cE "$end_re" "$f" || true)
  if [ "$begins" = 0 ] && [ "$ends" = 0 ]; then
    say "$repo: no house block yet (its baseline job or the rollout adds it)"
    continue
  fi
  b=$(grep -nE "$begin_re" "$f" | head -1 | cut -d: -f1 || true)
  e=$(grep -nE "$end_re" "$f" | head -1 | cut -d: -f1 || true)
  if [ "$begins" != 1 ] || [ "$ends" != 1 ] || [ "$e" -le "$b" ]; then
    say "$repo: the house block's markers are mangled"
    echo "$repo" >>"$tmp/mangled"
    continue
  fi
  sed -n "${b},${e}p" "$f" | cmp -s - "$RULES" && continue

  # The new AGENTS.md: everything outside the markers as it was.
  { head -n $((b - 1)) "$f"; cat "$RULES"; tail -n +$((e + 1)) "$f"; } >"$tmp/new"

  open=$(gh pr list -R "$repo" --state open --json number,headRefName \
    --jq '[.[] | select(.headRefName | startswith("house/rules-"))]')
  if [ "$(jq -r --arg b "$branch" '[.[] | select(.headRefName == $b)] | length' <<<"$open")" != 0 ]; then
    continue
  fi
  if [ -n "${DRY_RUN:-}" ]; then
    say "$repo: would open a PR updating its house block to $rev"
    continue
  fi

  sha=$(gh api "repos/$repo/git/ref/heads/$base" --jq .object.sha)
  gh api "repos/$repo/git/refs" -f ref="refs/heads/$branch" -f sha="$sha" >/dev/null 2>&1 || true
  fsha=$(gh api "repos/$repo/contents/AGENTS.md?ref=$branch" --jq .sha)
  gh api -X PUT "repos/$repo/contents/AGENTS.md" -f branch="$branch" -f sha="$fsha" \
    -f message="house: the house rules block, as burrow has it now" \
    -f content="$(base64 -w0 "$tmp/new")" >/dev/null
  url=$(gh pr create -R "$repo" --draft --base "$base" --head "$branch" \
    --title "house: the house rules block, as burrow has it now" \
    --body "burrow's house/rules.md changed (now $rev). This rewrites only what's between the house markers in AGENTS.md, so it matches. Change the rules in FieldmouseWorks/burrow, not here.")
  say "$repo: opened $url"
  # The older rules PRs this one replaces.
  jq -r '.[].number' <<<"$open" | while read -r n; do
    gh pr close "$n" -R "$repo" --delete-branch --comment "Replaced by $url." >/dev/null || true
  done
done 3<"$tmp/repos"

[ -n "${DRY_RUN:-}" ] && exit 0
if [ -s "$tmp/mangled" ]; then
  "$NOTIFY" open house-markers "The house block's markers are broken" \
    "house-sync can't find exactly one house block in these repos' AGENTS.md, so it can't keep them current: $(paste -sd, "$tmp/mangled" | sed 's/,/, /g'). Put back one begin and one end marker line, as burrow's house/rules.md has them."
else
  "$NOTIFY" close house-markers "Every house block is readable again."
fi
