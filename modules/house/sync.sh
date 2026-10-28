# house-sync: keep every repo's copies of the house blocks (the files in
# burrow's house/) the same as burrow's. A repo whose blocks differ gets one
# PR that rewrites only what's between their markers, from an ordinary
# branch, so the steward reviews and lands it like any other PR. (HOUSE,
# CURRENT, LOCK, OWNERS, EXCLUDE, DRY_RUN and NOTIFY come from ../house.nix.)
#
#   house-sync                     one pass over every repo
#   house-sync OWNER/REPO          one over that repo (a push to its default branch)
#   house-sync status [OWNER/REPO] what a pass would do, changing nothing

set -euo pipefail
if [ "${1:-}" = status ]; then
  DRY_RUN=1
  shift
fi
only=${1:-}

# One pass at a time, and only for the blocks the system has now: a pass
# from before a switch, still running or still waiting here, would close
# the PR for the new blocks as one for older ones. A pass that waited for the
# lock checks the blocks after it, when it would start.
if [ -z "${DRY_RUN:-}" ]; then
  exec 9>"$LOCK"
  flock 9
fi
if ! diff -rq "$HOUSE" "$CURRENT" >/dev/null 2>&1; then
  echo "house-sync: the system has other house blocks than this pass; the pass for those does the work"
  exit 0
fi

# One block a file: house/NAME.md, its first and last lines its markers,
# <!-- house:NAME vN begin ... --> and <!-- house:NAME end -->.
blocks=()
for f in "$HOUSE"/*.md; do
  blocks+=("$(basename "$f" .md)")
done
rev=$(cd "$HOUSE" && sha256sum -- *.md | sha256sum | cut -c1-8)
branch="house/sync-$rev"
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
: >"$tmp/repos"
if [ -n "$only" ]; then
  # One repo: only one of the owners', as a full pass would list it.
  if ! gh api "repos/$only" --jq 'select((.archived or .fork) | not) | "\(.full_name) \(.default_branch) \(.owner.login)"' >"$tmp/one"; then
    say "$only: couldn't read it, trying again next pass"
    exit 1
  fi
  if read -r repo base login <"$tmp/one"; then
    for owner in "${owners[@]}"; do
      if [ "${owner,,}" = "${login,,}" ]; then
        echo "$repo $base" >"$tmp/repos"
      fi
    done
  fi
  [ -s "$tmp/repos" ] || say "$only: not a repo it keeps"
else
  for owner in "${owners[@]}"; do
    # An owner whose list can't be read is a failure too; the others go on.
    if ! gh repo list "$owner" --no-archived --source --limit 1000 \
      --json nameWithOwner,defaultBranchRef \
      --jq '.[] | select(.defaultBranchRef != null) | "\(.nameWithOwner) \(.defaultBranchRef.name)"' >>"$tmp/repos"; then
      say "$owner: couldn't list its repos, trying again next pass"
      echo "$owner (its repo list)" >>"$tmp/failed"
    fi
  done
fi

# Every gh call below is checked by hand: set -e doesn't reach into a
# function called with ||, and one repo's failure shouldn't end the pass.

# Close the open PRs for older rules, so none of them can land after the
# current one. Any close that fails fails the repo, and the next pass retries.
close_older() {
  local repo=$1 why=$2 old n failed=0
  old=$(gh pr list -R "$repo" --state open --limit 1000 --json number,headRefName |
    jq -r --arg b "$branch" '.[] | select((.headRefName | startswith("house/sync-")) and .headRefName != $b) | .number') ||
    return 1
  for n in $old; do
    if [ -n "${DRY_RUN:-}" ]; then
      say "$repo: would close #$n ($why)"
    else
      gh pr close "$n" -R "$repo" --delete-branch --comment "$why" >/dev/null || failed=1
    fi
  done
  return $failed
}

sync_repo() {
  local repo=$1 base=$2 f="$tmp/AGENTS.md" sha fsha name begin_re end_re begins ends b e found=0 url
  # One snapshot of the default branch: the file is read at this commit and
  # the branch starts from it, so the PR can't undo a change made meanwhile.
  sha=$(gh api "repos/$repo/git/ref/heads/$base" --jq .object.sha) || return 1
  if ! gh api "repos/$repo/contents/AGENTS.md?ref=$sha" >"$tmp/file.json" 2>"$tmp/err"; then
    # No AGENTS.md yet: the baseline job writes it, block included. Any other
    # error is a failure, not a missing file.
    grep -q 'HTTP 404' "$tmp/err" && return 0
    cat "$tmp/err" >&2
    return 1
  fi
  fsha=$(jq -r .sha "$tmp/file.json") || return 1
  jq -r .content "$tmp/file.json" | base64 -d >"$f" || return 1

  # The new AGENTS.md: each block it has, as burrow has it, and everything
  # outside the markers as it was. A block it doesn't have yet stays out (its
  # baseline job or the rollout adds it). A block needs one begin marker, one
  # end marker after it, and no other block's marker between them.
  cp "$f" "$tmp/new"
  for name in "${blocks[@]}"; do
    begin_re="^<!-- house:$name v[0-9]+ begin"
    end_re="^<!-- house:$name end -->\$"
    begins=$(grep -cE "$begin_re" "$tmp/new" || true)
    ends=$(grep -cE "$end_re" "$tmp/new" || true)
    [ "$begins" = 0 ] && [ "$ends" = 0 ] && continue
    b=$(grep -nE "$begin_re" "$tmp/new" | head -1 | cut -d: -f1 || true)
    e=$(grep -nE "$end_re" "$tmp/new" | head -1 | cut -d: -f1 || true)
    if [ "$begins" != 1 ] || [ "$ends" != 1 ] || [ "$e" -le "$b" ] ||
      [ "$(awk -v b="$b" -v e="$e" 'NR > b && NR < e && /^<!-- house:[^ ]+ (v[0-9]+ begin|end -->)/ { n++ } END { print n + 0 }' "$tmp/new")" != 0 ]; then
      say "$repo: the markers of its house:$name block are mangled"
      echo "$repo" >>"$tmp/mangled"
      return 0
    fi
    { head -n $((b - 1)) "$tmp/new"; cat "$HOUSE/$name.md"; tail -n +$((e + 1)) "$tmp/new"; } >"$tmp/next"
    mv "$tmp/next" "$tmp/new"
    found=1
  done
  if [ "$found" = 0 ]; then
    say "$repo: no house block yet (its baseline job or the rollout adds it)"
    return 0
  fi
  if cmp -s "$f" "$tmp/new"; then
    close_older "$repo" "AGENTS.md on $base already has the current house blocks ($rev)."
    return
  fi

  # Already open for these blocks: only the cleanup may be left to do.
  url=$(gh pr list -R "$repo" --state open --head "$branch" --json url --jq '.[0].url // ""') || return 1
  if [ -n "$url" ]; then
    close_older "$repo" "Replaced by $url."
    return
  fi
  if [ -n "${DRY_RUN:-}" ]; then
    say "$repo: would open a PR updating its house blocks to $rev"
    close_older "$repo" "Replaced by the PR for $rev."
    return
  fi

  # The branch is ours: if a run before this one left it, start it over.
  gh api "repos/$repo/git/refs" -f ref="refs/heads/$branch" -f sha="$sha" >/dev/null 2>&1 ||
    gh api -X PATCH "repos/$repo/git/refs/heads/$branch" -f sha="$sha" -F force=true >/dev/null ||
    return 1
  gh api -X PUT "repos/$repo/contents/AGENTS.md" -f branch="$branch" -f sha="$fsha" \
    -f message="house: the house blocks, as burrow has them now" \
    -f content="$(base64 -w0 "$tmp/new")" >/dev/null || return 1
  url=$(gh pr create -R "$repo" --base "$base" --head "$branch" \
    --title "house: the house blocks, as burrow has them now" \
    --body "burrow's house/ changed (now $rev). This rewrites only what's between the house markers in AGENTS.md, so each block matches. Change the blocks in FieldmouseWorks/burrow, not here.") ||
    return 1
  say "$repo: opened $url"
  close_older "$repo" "Replaced by $url."
}

# fd 3, so nothing in the loop can eat the list from stdin.
while read -r repo base <&3; do
  excluded "$repo" && continue
  if ! sync_repo "$repo" "$base"; then
    say "$repo: failed, trying again next pass"
    echo "$repo" >>"$tmp/failed"
  fi
done 3<"$tmp/repos"

[ -n "${DRY_RUN:-}" ] && exit 0
# The owner's notices are about every repo, so only a full pass changes them.
# One repo's pass that failed says so to the journal, and the next full pass
# puts it in the notice.
if [ -n "$only" ]; then
  [ -s "$tmp/failed" ] && exit 1
  exit 0
fi
if [ -s "$tmp/mangled" ]; then
  "$NOTIFY" open house-markers "The house blocks' markers are broken" \
    "house-sync can't find one whole copy of each house block in these repos' AGENTS.md, so it can't keep them current: $(paste -sd, "$tmp/mangled" | sed 's/,/, /g'). Put back one begin and one end marker line around each block, as burrow's house/ has them (the journal names the block)."
elif ! [ -s "$tmp/failed" ]; then
  # Only once every repo was read: one that failed may still be broken.
  "$NOTIFY" close house-markers "Every house block is readable again."
fi
if [ -s "$tmp/failed" ]; then
  "$NOTIFY" open house-sync "house-sync failed on some repos" \
    "These couldn't be checked or updated on the last pass (see journalctl -u house-sync): $(paste -sd, "$tmp/failed" | sed 's/,/, /g'). It tries again on the next pass: a push to the repo's default branch, or the hourly catch-up."
else
  "$NOTIFY" close house-sync "house-sync got through every repo again."
fi
