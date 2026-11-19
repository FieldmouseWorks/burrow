# The shared health contract, imported by modules that add checks too.
{ lib, ... }:
{
  options.burrow.updates = {
    requiredHealth = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Health checks that must pass after a switch even if they failed before it. A missing required check fails too; proj adds its turn configuration check.";
    };
    health = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = { website = "curl -fsS -o /dev/null https://example.com/"; };
      description = ''
        Shell commands that exit 0 while the machine is healthy, by name.
        HOST-update compares them before and after every switch: one that
        passed before and fails after reverts the switch, and burrow.heartbeat
        tells the owner about one that keeps failing. Checks in requiredHealth
        must always pass after a switch. Failure notices include bounded
        diagnostics from failed checks. burrow adds its own (sshd, the nix
        daemon, the operator, burrow-events, ZFS, DNS, GitHub, Claude Code
        and its login).
      '';
    };
    healthExplains = lib.mkOption {
      type = lib.types.attrsOf (lib.types.listOf lib.types.str);
      default = { };
      example = { claude-login = [ "check:operator" "user-unit:claude-rc@*.service" ]; };
      description = ''
        By health check: what can't pass while it fails, as the report says
        it (`check:NAME`, `unit:NAME` or `user-unit:NAME`, `*` standing for
        any run of characters). While that check fails after a switch, these
        don't count against the switch: a cause already failing before it
        (Claude Code logged out, say) doesn't revert a good deploy because
        the switch restarted what needs it. The check itself counts as any
        other. Only a check already failing before the switch (or new with
        it) explains; a required check is never excused.
      '';
    };
  };
}
