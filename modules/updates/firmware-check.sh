# firmware-check: tell the owner when there's firmware to flash. Flashing
# stays their call (a bad flash can brick a board with no console), so this
# only looks. Its job acts on GitHub as the machine's own App, APP, when it
# has one. (Everything in capitals comes from ../updates.nix.)
#
#   firmware-check            daily: what fwupd (LVFS) offers for this box
#   firmware-check --review   monthly: a Claude job checks what LVFS doesn't
#                             carry (board BIOS, NVMe, Intel ME or AMD PSP)

[ "$(id -u)" = 0 ] || exec sudo "$0" "$@"

state=$DIR
as_user() { runuser -u "$BURROW_USER" -- env HOME="$HOME_DIR" XDG_RUNTIME_DIR="/run/user/$USER_UID" "$@"; }

if [ "${1:-}" = --review ]; then
  bios=$(dmidecode -s bios-version 2>/dev/null || echo unknown)
  bios_date=$(dmidecode -s bios-release-date 2>/dev/null || echo unknown)
  board=$(dmidecode -s baseboard-product-name 2>/dev/null || echo unknown)
  nvme=$(nvme list -o json 2>/dev/null | jq -r '.Devices[] | "\(.ModelNumber) firmware \(.Firmware)"' | sort -u | paste -sd';' -) || nvme=unknown
  cpu=$(grep -m1 '^model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//') || cpu=unknown
  as_user proj run ${APP:+--app "$APP"} "$PROJECT" "Monthly firmware review. Don't flash or change anything; look and report.
$HOST: board $board, BIOS $bios ($bios_date), NVMe: $nvme. CPU: $cpu. fwupd's view: \`fwupdmgr get-devices\` and \`fwupdmgr get-updates\`.
Find the newest firmware the vendors publish for each ($REVIEW_HINTS; microcode itself arrives with NixOS). Say what's out of date and whether any of it fixes a security problem that matters for a server on the internet. If something is worth flashing, run \`$NOTIFY open firmware-review \"title\" \"what, why, and how it would be flashed on $HOST\"\` so $OWNER sees it; flashing is their call. If everything is current, \`$NOTIFY close firmware-review \"current as of $(date +%F)\"\`." >/dev/null
  exit 0
fi

fwupdmgr refresh --force --no-unreported-check --no-metadata-check >/dev/null 2>&1 || true
out=$(fwupdmgr get-updates --json --no-unreported-check --no-metadata-check 2>/dev/null || true)
[ -n "$out" ] || out='{}'
list=$(jq -r '.Devices[]? | select((.Releases // []) | length > 0)
  | "- \(.Name): \(.Version) -> \(.Releases[0].Version) \(.Releases[0].Summary // "")"' <<<"$out" 2>/dev/null || true)

if [ -n "$list" ]; then
  if [ "$list" != "$(cat "$state/firmware-offered" 2>/dev/null)" ]; then
    echo "$list" >"$state/firmware-offered"
    "$NOTIFY" open firmware "Firmware update available" \
      "fwupd has firmware for $HOST:
$list

Flashing is your call. To go ahead, tell the $HOST operator \"flash it\"; it will check the release notes, snapshot, and run \`fwupdmgr update\`."
  fi
else
  rm -f "$state/firmware-offered"
  "$NOTIFY" close firmware "No firmware updates on offer any more."
fi
