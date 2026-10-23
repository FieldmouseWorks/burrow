# HOST-notify: one GitHub issue per problem the owner should know about, in
# REPO under the LABEL label. GitHub's app is how it reaches their phone.
# Repeats of the same problem don't pile up: the issue gets at most one comment
# a day, and it closes when the problem clears. (NOTIFY, REPO, ASSIGNEE, LABEL,
# DIR, BURROW_USER and HOME_DIR come from ../notify.nix.)
#
#   HOST-notify open KEY "title" "body"
#   HOST-notify close KEY "what fixed it"
#
# Never fails the caller: a notice that can't be sent is logged and dropped.
# With no REPO, every notice only goes to the journal.

repo=$REPO
state=$DIR

# gh needs the user's login.
if [ "$(id -un)" != "$BURROW_USER" ]; then
  mkdir -p "$state"
  chown "$BURROW_USER": "$state" 2>/dev/null || true
  exec runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" "$0" "$@"
fi

key=${2:?usage: $NOTIFY open|close KEY ...}
tag="[$key]"
if [ -z "$repo" ]; then
  echo "$NOTIFY $*" | systemd-cat -t "$NOTIFY" -p "$([ "$1" = open ] && echo warning || echo notice)"
  exit 0
fi

# Fails when GitHub can't be asked, so a close isn't mistaken for "no issue".
find_issue() {
  gh issue list -R "$repo" --label "$LABEL" --state open --search "\"$tag\" in:title" \
    --json number,title --jq "[.[] | select(.title | startswith(\"$tag\")) | .number] | first // empty" 2>/dev/null
}

{
  case $1 in
    open)
      title=${3:?} body=${4:?}
      n=$(find_issue)
      if [ -z "$n" ]; then
        gh label create "$LABEL" -R "$repo" --color B60205 --description "$NOTIFY's notices" >/dev/null 2>&1 || true
        gh issue create -R "$repo" --label "$LABEL" ${ASSIGNEE:+--assignee "$ASSIGNEE"} --title "$tag $title" --body "$body" &&
          date +%s >"$state/$key"
      elif [ $(($(date +%s) - $(cat "$state/$key" 2>/dev/null || echo 0))) -gt 86400 ]; then
        gh issue comment "$n" -R "$repo" --body "Still happening: $body" &&
          date +%s >"$state/$key"
      fi
      ;;
    close)
      # Only what this opened (saves an API call every run).
      [ -e "$state/$key" ] || exit 0
      # Forget it only once it's closed (or gone), so a failed close is
      # retried by the next one.
      n=$(find_issue) &&
        { [ -z "$n" ] || gh issue close "$n" -R "$repo" --comment "${3:-Cleared.}"; } &&
        rm -f "$state/$key"
      ;;
    *)
      echo "usage: $NOTIFY open|close KEY ..." >&2
      exit 2
      ;;
  esac
} || echo "$NOTIFY $1 $key: couldn't reach GitHub" | systemd-cat -t "$NOTIFY" -p warning
exit 0
