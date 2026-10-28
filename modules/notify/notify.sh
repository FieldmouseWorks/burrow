# HOST-notify: one GitHub issue per problem the owner should know about, in
# REPO under the LABEL label. GitHub's app is how it reaches their phone.
# Repeats of the same problem don't pile up: the issue gets at most one comment
# a day, and it closes when the problem clears. (NOTIFY, REPO, ASSIGNEE, LABEL,
# DIR, BURROW_USER and HOME_DIR come from ../notify.nix.)
#
#   HOST-notify open KEY "title" "body"
#   HOST-notify close KEY "what fixed it"
#   HOST-notify pending KEY
#
# Never fails the caller: a notice that can't be sent is logged and dropped.
# With no REPO, every notice only goes to the journal. `pending` answers with
# its exit status, asking nothing of GitHub: 0 while a notice this asked for
# may be open, 1 once GitHub has shown it closed or never opened, so a caller
# can retry a close until it has.

repo=$REPO
state=$DIR

# gh needs the user's login.
if [ "$(id -un)" != "$BURROW_USER" ]; then
  mkdir -p "$state"
  chown "$BURROW_USER": "$state" 2>/dev/null || true
  exec runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" "$0" "$@"
fi

key=${2:?usage: $NOTIFY open|close|pending KEY ...}
tag="[$key]"
if [ "$1" = pending ]; then
  [ -e "$state/$key" ]
  exit
fi
if [ -z "$repo" ]; then
  echo "$NOTIFY $*" | systemd-cat -t "$NOTIFY" -p "$([ "$1" = open ] && echo warning || echo notice)"
  exit 0
fi

# KEY's open issues, a number a line, read from the issues themselves: the
# search index lags a new one. Fails when GitHub can't be asked, so that isn't
# mistaken for "no issue".
find_issues() {
  gh api -X GET --paginate "repos/$repo/issues" -f state=open -f labels="$LABEL" -F per_page=100 \
    --jq ".[] | select(.pull_request == null and (.title | startswith(\"$tag\"))) | .number" 2>/dev/null
}

# Closes each issue in $1 (a number a line), saying $2.
close_issues() {
  local n
  while read -r n; do
    [ -z "$n" ] || gh issue close "$n" -R "$repo" --comment "$2" || return
  done <<<"$1"
}

{
  case $1 in
    open)
      title=${3:?} body=${4:?}
      n=$(find_issues) &&
        if [ -z "$n" ]; then
          gh label create "$LABEL" -R "$repo" --color B60205 --description "$NOTIFY's notices" >/dev/null 2>&1 || true
          # Kept before it's asked for: GitHub can open it and the answer
          # still be lost, and then only this says it needs closing.
          date +%s >"$state/$key" &&
            gh issue create -R "$repo" --label "$LABEL" ${ASSIGNEE:+--assignee "$ASSIGNEE"} --title "$tag $title" --body "$body"
        elif [ $(($(date +%s) - $(cat "$state/$key" 2>/dev/null || echo 0))) -gt 86400 ]; then
          gh issue comment "${n%%$'\n'*}" -R "$repo" --body "Still happening: $body" &&
            date +%s >"$state/$key"
        fi
      ;;
    close)
      # Only what this asked for (saves an API call every run).
      [ -e "$state/$key" ] || exit 0
      # Forget it only once none is open, so a failed close is retried by
      # the next one.
      n=$(find_issues) &&
        close_issues "$n" "${3:-Cleared.}" &&
        rm -f "$state/$key"
      ;;
    *)
      echo "usage: $NOTIFY open|close|pending KEY ..." >&2
      exit 2
      ;;
  esac
} || echo "$NOTIFY $1 $key: couldn't reach GitHub" | systemd-cat -t "$NOTIFY" -p warning
exit 0
