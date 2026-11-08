# The fixture's log records requests and decoded fields; api-log keeps argv.
echo "$*" >>"$TEST/api-log"
if [ "$1" != api ]; then echo '{}'; exit 0; fi
[ "$2 $3" = '-i -X' ] || exit 2
method=$4 path=$5
body='{}'
[ "$6" != --input ] || body=$(cat)
if [ -e "$TEST/once-502" ]; then
  rm "$TEST/once-502"
  printf 'HTTP/2.0 502 Bad Gateway\n\n{}'; exit 1
fi
if [ "$path" = graphql ]; then
  # The GraphQL fixture reads variables as fields for its jq data builder.
  set -- api graphql
  for key in owner name n cursor; do
    value=$(printf '%s' "$body" | jq -r ".variables.$key // empty")
    [ -z "$value" ] || set -- "$@" "$key=$value"
  done
  result=$("$TEST/graphql" "$@") || exit 1
  printf 'HTTP/2.0 200 OK\n\n%s' "$result"; exit 0
fi
if [ "$method" = GET ]; then
  request="api $path"
  case "$path" in */comments*) request="api --paginate --slurp $path" ;; esac
else
  request="api -X $method $path"
  fields=$(printf '%s' "$body" | jq -r 'to_entries[] | if (.value|type)=="array" then "-f " + .key + "[]=" + .value[0] else "-f " + .key + "=" + .value end')
  [ -z "$fields" ] || request="$request $(printf '%s' "$fields" | tr '\n' ' ')"
  case "$path" in */comments) request="api $path -F body=@-" ;; esac
fi
echo "$request" >>"$TEST/log"
[ ! -x "$TEST/hook" ] || "$TEST/hook" "$request"
if [ -f "$TEST/fail" ] && printf '%s' "$request" | grep -qF -f "$TEST/fail"; then exit 1; fi
if [ "$method" != GET ]; then
  case "$path" in */comments) printf '%s' "$body" | jq -r .body >>"$TEST/posted" ;; esac
  printf 'HTTP/2.0 200 OK\n\n{}'; exit 0
fi
page=1; case "$path" in *\?page=*|*\&page=*) page=${path##*page=} ;; esac
path=${path%%\?*}
printf 'HTTP/2.0 200 OK\n'
case "$path" in
  */comments)
    pages=$(cat "$TEST/comments" 2>/dev/null || echo '[[]]')
    count=$(printf '%s' "$pages" | jq length)
    if [ "$page" -lt "$count" ]; then printf 'Link: <https://api.github.com/%s?page=%s>; rel="next"\n' "$path" "$((page+1))"; fi
    printf '\n'; printf '%s' "$pages" | jq ".[$((page-1))] // []" ;;
  *) printf '\n'; cat "$TEST/$(printf '%s' "$path" | tr / _)" 2>/dev/null || echo '{}' ;;
esac
