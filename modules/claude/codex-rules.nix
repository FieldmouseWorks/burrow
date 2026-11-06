# Execpolicy has literal command prefixes, not Bash globs. End a rule at
# the last complete word before a wildcard, conservatively forbidding that
# command family (fwupdmgr, for example) rather than missing its variants.
# The CLI cannot express an executable glob such as mkfs*. These common
# variants are explicit rules; an unlisted executable suffix cannot be
# covered by this CLI policy. Keep that limitation visible when adding tools.
{ lib, patterns }:
let
  mkfs = [ "mkfs" "mkfs.bfs" "mkfs.btrfs" "mkfs.cramfs" "mkfs.erofs" "mkfs.exfat" "mkfs.ext2" "mkfs.ext3" "mkfs.ext4" "mkfs.f2fs" "mkfs.fat" "mkfs.hfs" "mkfs.hfsplus" "mkfs.jfs" "mkfs.minix" "mkfs.msdos" "mkfs.nilfs2" "mkfs.ntfs" "mkfs.reiserfs" "mkfs.ubifs" "mkfs.udf" "mkfs.vfat" "mkfs.xfs" ];
  completeWords = words:
    if words == [ ] || lib.hasInfix "*" (builtins.head words) then [ ]
    else [ (builtins.head words) ] ++ completeWords (builtins.tail words);
  prefixes = pattern:
    let
      words = lib.splitString " " pattern;
      complete = completeWords words;
    in
    if complete != [ ] then [ complete ]
    else if pattern == "mkfs*" then map (n: [ n ]) mkfs
    else throw "Codex has no execpolicy rule for R2 pattern: ${pattern}";
  rules = lib.concatMap (p: map (prefix: { pattern = p; inherit prefix; }) (prefixes p)) patterns;
  render = rules: lib.concatMapStrings (r:
    "prefix_rule(pattern = ${builtins.toJSON r.prefix}, decision = \"forbidden\")\n"
    + "prefix_rule(pattern = ${builtins.toJSON ([ "sudo" ] ++ r.prefix)}, decision = \"forbidden\")\n"
  ) rules;
in
{ inherit rules render; text = render rules; }
