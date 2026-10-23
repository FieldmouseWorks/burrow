# renovate-sweep: Renovate PRs that can't merge themselves get a Claude.
#
# Renovate merges its own PRs once CI is green. The ones it can't (CI red,
# or a major version, which the owner merges) would otherwise sit there.
# Every few hours this hands each such PR to a Claude job in that project,
# which fixes it on the PR's branch so it can merge. Once per PR head, so a
# fix that's pushed gets a fresh look only if it's still stuck. (DIR, SRC,
# OWNER and NOTIFY come from ../updates.nix.)

state=$DIR/renovate-sweep
src=$SRC
mkdir -p "$state"
ready=$(mktemp)
trap 'rm -f "$ready"' EXIT

for d in "$src"/*/; do
  p=$(basename "$d")
  url=$(git -C "$d" remote get-url origin 2>/dev/null) || continue
  repo=$(sed -E 's#^(https://github.com/|git@github.com:)##; s#\.git$##' <<<"$url")
  [[ $repo == */* ]] || continue

  gh pr list -R "$repo" --state open --search "head:renovate/" \
    --json number,title,headRefName,headRefOid,labels,statusCheckRollup 2>/dev/null |
    jq -c '.[]' |
    while read -r pr; do
      n=$(jq -r .number <<<"$pr")
      sha=$(jq -r .headRefOid <<<"$pr")
      title=$(jq -r .title <<<"$pr")
      branch=$(jq -r .headRefName <<<"$pr")
      # Check runs carry a conclusion (empty while running), commit statuses a state.
      red=$(jq -r '[.statusCheckRollup[]? | (.conclusion // .state // "" | ascii_upcase)
        | select(. == "FAILURE" or . == "ERROR" or . == "TIMED_OUT" or . == "STARTUP_FAILURE")] | length' <<<"$pr")
      major=$(jq -r '[.labels[]?.name] | index("major") != null' <<<"$pr")
      pending=$(jq -r '[.statusCheckRollup[]? | (.conclusion // .state // "" | ascii_upcase)
        | select(. == "" or . == "PENDING" or . == "EXPECTED")] | length' <<<"$pr")
      if [ "$major" = true ] && [ "$red" -eq 0 ] && [ "$pending" -eq 0 ]; then
        echo "- $title: https://github.com/$repo/pull/$n" >>"$ready"
      fi
      [ "$red" -gt 0 ] || [ "$major" = true ] || continue
      key="$state/${repo//\//_}-$n"
      [ "$(cat "$key" 2>/dev/null)" != "$sha" ] || continue
      echo "$sha" >"$key"
      if [ "$major" = true ]; then
        why="it's a major version bump"
        [ "$red" -eq 0 ] || why="$why and its CI fails"
      else
        why="its CI fails"
      fi
      proj run "$p" "Renovate's PR #$n in $repo (\"$title\", branch $branch) can't merge itself: $why. Make it mergeable: check out the branch in your own worktree (git worktree add), fix whatever the update breaks (code, config, lockfile), run the tests, and push to the same branch. Renovate merges minor and patch PRs itself once CI is green. A major one waits for $OWNER: leave a short comment on the PR with the breaking changes that touch this project and what you changed for them, and don't merge it. If it can't or shouldn't be done (an upstream regression, a migration that needs a decision), comment on the PR saying why, in one or two lines, and close it only if it's a dead end." >/dev/null &&
        echo "$repo#$n: handed to Claude ($why)"
    done || true
done

# Green major updates are the owner's to merge: one notice listing them all.
if [ -s "$ready" ]; then
  "$NOTIFY" open majors "Major updates ready to merge" "These passed CI, and a Claude job has read their breaking changes (see its comment on each PR). Merging is one tap each:
$(cat "$ready")"
else
  "$NOTIFY" close majors "No major updates waiting."
fi
