# Every file in /etc/claude-code is a copy, never a link into the store.
# Claude Code watches the directory each of its settings files really lives
# in, and environment.etc's default is a symlink that resolves to a file
# sitting directly in /nix/store, so every Claude watched the whole store. On
# a busy machine that's over 100k entries, and the Claudes holding it open
# grew to several gigabytes each. A mode makes NixOS copy the file instead.
#
# Imported by the host's Claude (../claude.nix) and the sandboxes'
# (../agents/guest.nix).
{ config, lib, ... }:
let
  linked = lib.filter
    (f: f.enable && (f.target == "claude-code" || lib.hasPrefix "claude-code/" f.target) && f.mode == "symlink")
    (lib.attrValues config.environment.etc);
in
{
  assertions = [{
    assertion = linked == [ ];
    message = ''
      These would be symlinks into the Nix store, and Claude Code would watch
      all of /nix/store through them: ${lib.concatMapStringsSep ", " (f: "/etc/${f.target}") linked}.
      Give each a mode (mode = "0444") so NixOS copies it.'';
  }];
}
