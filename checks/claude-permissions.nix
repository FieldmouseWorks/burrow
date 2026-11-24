# The managed settings let the standing-authority commands through
# (modules/engines/anthropic/standing.nix) and nothing near them, and still
# ask at every one-way door: anthropic/r2.nix and extraAsk, each with and
# without sudo, with the guard hook in front. Commands are judged the way Claude Code reads
# its Bash rules (`*` is any text, a trailing ` *` also takes the bare
# command; ask beats allow), so the check tests itself on mutants: no allow
# rules (the settings before standing.nix), an allow rule as wide as `gh`,
# and an ask rule dropped must each fail it.
{ self, pkgs, ... }:
let
  inherit (pkgs) lib;
  extraAsk = [ "terraform destroy *" ];
  machine = self.nixosConfigurations.example.extendModules {
    modules = [ { burrow.claude.extraAsk = extraAsk; } ];
  };
  settings = builtins.fromJSON (builtins.unsafeDiscardStringContext machine.config.environment.etc."claude-code/managed-settings.json".text);

  regex = pattern:
    let
      bare = lib.hasSuffix " *" pattern;
      body = if bare then lib.removeSuffix " *" pattern else pattern;
      glob = lib.concatMapStringsSep ".*" lib.escapeRegex (lib.splitString "*" body);
    in
    if bare then "${glob}( .*)?" else glob;
  matches = rule: cmd:
    lib.hasPrefix "Bash(" rule && lib.hasSuffix ")" rule
    && builtins.match (regex (lib.removeSuffix ")" (lib.removePrefix "Bash(" rule))) cmd != null;
  decide = s: cmd:
    if lib.any (r: matches r cmd) (s.permissions.ask or [ ]) then "ask"
    else if lib.any (r: matches r cmd) (s.permissions.allow or [ ]) then "allow"
    else "check";

  head = "b95f9b5168f9e7d2c1a0b3e4f5a6b7c8d9e0f1a2";
  cases = {
    # Standing authority: no auto-mode check.
    allow = [
      ''proj reply remi-20261114-041300-AbCd "Peter signed in to GitHub in remi's browser; carry on."''
      "settings land FieldmouseWorks/wend#218 ${head}"
      "gh issue edit 1239 -R FieldmouseWorks/remi --remove-label needs-peter"
      "gh pr edit 218 -R FieldmouseWorks/wend --remove-label hold"
      "gh pr edit https://github.com/FieldmouseWorks/wend/pull/218 --remove-label hold --remove-label 'waiting: fix'"
      "gh run rerun 19283746501 -R FieldmouseWorks/wend --failed"
      "gh run rerun 19283746501 --job 5551234"
    ];
    # Near them, but not theirs: left to the session's mode.
    check = [
      "proj"
      "proj run wend 'merge #218'"
      "proj stop remi-20261114-041300-AbCd"
      "proj settings role review codex"
      "proj replay remi-20261114-041300-AbCd"
      "sudo proj reply remi-20261114-041300-AbCd yes"
      "settings allow FieldmouseWorks/wend"
      "settings pause FieldmouseWorks/remi"
      "settings landing FieldmouseWorks/wend#218 ${head}"
      "gh pr merge 218 -R FieldmouseWorks/wend --squash --delete-branch"
      "gh pr edit 218 -R FieldmouseWorks/wend --add-label hold"
      "gh pr edit 218 --title 'something else'"
      "gh pr review 218 --approve"
      "gh pr close 218"
      "gh issue edit 1239 --add-label claude"
      "gh issue edit 1239 --body 'rewritten'"
      "gh issue close 1239"
      "gh api -X DELETE repos/FieldmouseWorks/wend/issues/218/labels/hold"
      "gh api -X PUT repos/FieldmouseWorks/wend/pulls/218/merge"
      "gh label delete hold -R FieldmouseWorks/wend"
      "gh run cancel 19283746501"
      "gh run delete 19283746501"
      "gh workflow run flake.yml -R FieldmouseWorks/wend"
      "gh run rerunx 19283746501"
    ];
    # One-way doors still ask, sudo or not, configured ones too.
    ask = [
      "zfs destroy rpool/safe/data"
      "sudo zfs destroy -r rpool/safe/home"
      "sudo zpool destroy rpool"
      "mkfs.ext4 /dev/nvme0n1p2"
      "gh repo delete FieldmouseWorks/wend --yes"
      "gh release delete v1 -R FieldmouseWorks/wend"
      "git push --force origin main"
      "rclone purge gdrive:remi-backup"
      "sudo fwupdmgr update"
      "terraform destroy -auto-approve"
      "sudo terraform destroy"
    ];
  };
  wrong = s: lib.concatLists (lib.mapAttrsToList (want: cmds:
    map (cmd: "${cmd}: ${decide s cmd}, not ${want}") (lib.filter (cmd: decide s cmd != want) cmds)) cases);

  r2 = lib.concatMap (c: [ "Bash(${c})" "Bash(sudo ${c})" ]) ((import ../modules/engines/anthropic/r2.nix) ++ extraAsk);
  sameSet = a: b: lib.sort lib.lessThan a == lib.sort lib.lessThan b;
  guarded = s: lib.any (h: h.matcher == "Bash"
    && lib.any (x: lib.hasSuffix "/bin/claude-guard" x.command) h.hooks) (s.hooks.PreToolUse or [ ]);
  problems = s:
    wrong s
    ++ lib.optional (!sameSet (s.permissions.ask or [ ]) r2) "the ask rules aren't R2 and extraAsk, each with and without sudo"
    ++ lib.optional ((s.permissions.defaultMode or null) != "bypassPermissions") "defaultMode isn't bypassPermissions"
    ++ lib.optional (!guarded s) "no claude-guard PreToolUse hook on Bash";

  permit = s: f: s // { permissions = f s.permissions; };
  mutants = {
    "no allow rules" = permit settings (p: removeAttrs p [ "allow" ]);
    "an allow rule for all of gh" = permit settings (p: p // { allow = (p.allow or [ ]) ++ [ "Bash(gh *)" ]; });
    "an ask rule dropped" = permit settings (p: p // { ask = builtins.tail p.ask; });
  };
  passed = lib.attrNames (lib.filterAttrs (_: s: problems s == [ ]) mutants);
  report = lib.concatMapStrings (p: "  ${p}\n");
in
pkgs.runCommand "claude-permissions" { } ''
  ${lib.optionalString (problems settings != [ ]) ''
    printf 'The managed settings are wrong:\n%s' ${lib.escapeShellArg (report (problems settings))} >&2
    exit 1
  ''}
  ${lib.optionalString (passed != [ ]) ''
    printf 'These mutants passed the check:\n%s' ${lib.escapeShellArg (report passed)} >&2
    exit 1
  ''}
  printf '%s\n' ${lib.escapeShellArgs (lib.mapAttrsToList (name: s: "${name}:\n${report (problems s)}") mutants)}
  touch $out
''
