# Where a turn stands in proj's line (modules/proj/line.sh's entry): one whose
# job was started --since a time (the steward's, from when its PR was opened)
# stands from then, ahead of turns that came before it but whose work began
# later, with --first ones ahead of the rest, and the names sort that way.
#
#   bash tests/proj-line.sh modules/proj/line.sh
set -euo pipefail
# shellcheck source=/dev/null
. "$1"
jobs=$(mktemp -d)
export jobs
trap 'rm -rf "$jobs"' EXIT
mkdir "$jobs/new" "$jobs/old-pr" "$jobs/asked"
new=$(entry new normal)
echo 1792800000 >"$jobs/old-pr/since"
old=$(entry old-pr normal)
asked=$(entry asked first)
[ "$old" = 2-1792800000000000000-old-pr ] || { echo "wanted old-pr's place from its --since, got $old" >&2; exit 1; }
[[ $new =~ ^2-[0-9]{19}-new$ ]] || { echo "wanted new's place from now, got $new" >&2; exit 1; }
got=$(printf '%s\n' "$new" "$old" "$asked" | LC_ALL=C sort | cut -d- -f3- | paste -sd ' ')
[ "$got" = "asked old-pr new" ] || { echo "wanted 'asked old-pr new', got '$got'" >&2; exit 1; }
echo "proj-line: ok"
