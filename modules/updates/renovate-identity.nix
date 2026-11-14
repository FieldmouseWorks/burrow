# Who Renovate is on GitHub, in one place: the App it acts as
# (burrow.updates.renovate.app), that App's bot, and the author its commits
# carry (gitAuthor). ../updates.nix runs Renovate as it; ../autoland.nix tells
# Renovate's PRs, dashboards and commits by it, and trusts its bot; ../proj.nix
# shows its App among the settings (settings/tracked.nix). Each imports this,
# so a steward, or proj, on a machine that doesn't run the updates module
# reads the same Renovate the machine's flake names. renovate.settings can't
# name another author (../updates.nix): gitAuthor here is the only one.
#
# An App's commits are its bot's on GitHub, and Renovate takes a commit by
# anyone but its gitAuthor for an edit (and stops rebasing that PR), so with
# an App the two are one fact: gitAuthor is the App's bot, worked out from its
# slug and accountId, and a value naming any other address fails evaluation.
inputs:
{ config, lib, ... }:
let
  inherit (lib) mkOption types;
  rcfg = config.burrow.updates.renovate;
  inherit (config.burrow.github) apps;
  app = if rcfg.app == null || !(apps ? ${rcfg.app}) then null else apps.${rcfg.app} // { name = rcfg.app; };
  login = "${app.slug}[bot]";
  email = "${toString app.accountId}+${login}@users.noreply.github.com";
  emailOf = author: let m = builtins.match ".*<([^>]*)>.*" author; in if m == null then null else lib.head m;
in
{
  key = "burrow/updates/renovate-identity";
  imports = [ { key = "burrow/github"; imports = [ (import ../github.nix inputs) ]; } ];

  options.burrow.updates.renovate = {
    app = mkOption {
      type = types.nullOr types.str;
      default = config.burrow.github.machinery;
      defaultText = lib.literalExpression "config.burrow.github.machinery";
      example = "renovate";
      description = ''
        The App, one of burrow.github.apps, Renovate acts as on GitHub: its
        runs spend that App's installations' allowance, so an App of its own
        keeps a long pass from stalling every other job. Its bot is the one
        the steward tells Renovate's PRs by. null: Renovate acts as
        burrow.user's own gh login.
      '';
    };
    gitAuthor = mkOption {
      type = types.nullOr types.str;
      default = if app == null then null else "${login} <${email}>";
      defaultText = lib.literalMD "`app`'s bot, `SLUG[bot] <ACCOUNTID+SLUG[bot]@users.noreply.github.com>`; null without one";
      example = "renovate <renovate@example.com>";
      description = ''
        Who Renovate's commits are by. With an App, its bot, which is the
        default: any other address fails evaluation, since GitHub makes the
        App's commits as its bot and Renovate would take them for edits.
        Without one, not the user's own email: Renovate keeps a branch that
        has a commit by someone other than this, and rebases over everything
        else, so fixes pushed as the user would be lost.
      '';
    };
    actor = mkOption {
      type = types.nullOr types.attrs;
      readOnly = true;
      internal = true;
      default = if app == null then null else app // { inherit login email; };
      description = ''
        `app`'s App, with its name, its bot's `login` as REST spells it
        (SLUG[bot]) and its commits' `email`: what everything that runs
        Renovate or recognises its work reads. null: the user's own login.
      '';
    };
  };

  config.assertions = [
    {
      assertion = rcfg.app == null || apps ? ${rcfg.app};
      message = "burrow.updates.renovate.app is ${toString rcfg.app}, which isn't one of burrow.github.apps.";
    }
    {
      assertion = app == null || (rcfg.gitAuthor != null && emailOf rcfg.gitAuthor == email);
      message = ''
        burrow.updates.renovate.gitAuthor is ${if rcfg.gitAuthor == null then "null" else rcfg.gitAuthor},
        but Renovate acts as the App ${rcfg.app}, whose commits GitHub makes as
        ${login} <${email}>. Renovate would take its own commits for edits:
        leave gitAuthor unset, or name that bot.'';
    }
  ];
}
