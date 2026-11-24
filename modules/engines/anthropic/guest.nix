# Claude in an agent sandbox (../../agents/guest.nix), the image
# ../anthropic.nix declares for its engine entries: nothing to ask, since the
# VM is the boundary, and the host's effort to start at.
{ effort }:
{ lib, ... }:
{
  imports = [ ./etc.nix ];

  # A copy, not a link into the store (./etc.nix).
  environment.etc."claude-code/managed-settings.json" = {
    mode = "0444";
    text = builtins.toJSON ({
      permissions.defaultMode = "bypassPermissions";
      skipDangerousModePermissionPrompt = true;
      env = {
        DISABLE_AUTOUPDATER = "1";
        # Telemetry and error reports only need more domains on the allowlist.
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1";
      };
    } // lib.optionalAttrs (effort != null) {
      effortLevel = effort;
    });
  };
}
