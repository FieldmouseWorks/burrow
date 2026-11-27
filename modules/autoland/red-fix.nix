# The branch a red default branch's fix comes from (burrow.autoland
# .redFixBranch): the flow's red_fix_branch is rendered from it
# (autoland/shadow.nix). Part of burrow.autoland, which imports it.
{ config, lib, ... }:
let
  inherit (lib) mkOption types;
  cfg = config.burrow.autoland;
  # The steward's own copy, RED_BRANCH in tasks.py, which its jobs' briefs
  # and its red watch read. It goes with the steward's Python
  # (FieldmouseWorks/burrow#1310); until then the build holds it to the
  # option, so the steward and the flow can't tell a red's fix apart.
  stewards = let
    found = lib.concatMap (l: let m = builtins.match ''RED_BRANCH = "([^"]*)"'' l; in lib.optionals (m != null) m)
      (lib.splitString "\n" (builtins.readFile ./tasks.py));
  in
  if builtins.length found == 1 then builtins.head found
  else throw "modules/autoland/tasks.py sets RED_BRANCH ${toString (builtins.length found)} times, not once";
in
{
  options.burrow.autoland.redFixBranch = mkOption {
    type = types.strMatching "[^/[:space:]][^[:space:]]*";
    default = "autoland/red-";
    description = ''
      The prefix of the branch a fix for a red default branch comes from,
      followed by the red commit: the steward's red jobs push there, a fix
      from one of its authors on it goes first, and Conary's flow knows it
      by it (red_fix_branch). The steward's Python has it too, and must
      say the same until it goes (FieldmouseWorks/burrow#1310).
    '';
  };

  config.assertions = lib.optional cfg.enable {
    assertion = cfg.redFixBranch == stewards;
    message = "burrow.autoland.redFixBranch is ${cfg.redFixBranch}, but the steward's RED_BRANCH (modules/autoland/tasks.py) is ${stewards}: they must name the same branch until the steward's Python goes (FieldmouseWorks/burrow#1310).";
  };
}
