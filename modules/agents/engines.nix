# burrow.agents.engines: what each engine brings to a sandbox. Declared
# apart from ../agents.nix so the module that owns an engine (Claude's is
# ../claude.nix) can add its entry on a machine without sandboxes, or when
# evaluated alone. ../agents.nix builds one guest image and one filtered
# proxy per entry: a VM started for an engine gets that engine's packages,
# its credential and its hosts, and none of another engine's.
{ lib, config, ... }:
let
  inherit (lib) mkOption types;
in
{
  key = "burrow/agents/engines";

  options.burrow.agents = {
    engines = mkOption {
      default = { };
      description = ''
        The engines a sandbox can be started for (`agent new --engine NAME`),
        by name. Each module that runs an engine declares its own entry; the
        machine adds what's its own, such as the credential's file.
      '';
      type = types.attrsOf (types.submodule {
        options = {
          packages = mkOption {
            type = types.listOf types.package;
            default = [ ];
            description = "What the engine's sandboxes have installed, on top of the shared guest.";
          };
          guestModules = mkOption {
            type = types.listOf types.deferredModule;
            default = [ ];
            description = "NixOS modules for this engine's sandbox image alone (its settings files, say).";
          };
          credentialFile = mkOption {
            type = types.nullOr types.str;
            default = null;
            example = "/run/secrets/claude-agent-token";
            description = ''
              The engine's login for the sandboxes, a file on the host. A VM for
              the engine gets a fresh copy at each start as the systemd
              credential `agent.credential`, and no other engine's. Give VMs a
              login of their own, never the host's (a rotating refresh token a
              VM refreshes can log out the host's sessions).
            '';
          };
          credentialEnv = mkOption {
            type = types.nullOr types.str;
            default = null;
            example = "CLAUDE_CODE_OAUTH_TOKEN";
            description = "The variable the credential is exported as, in every login shell in the VM.";
          };
          hosts = mkOption {
            type = types.listOf types.str;
            default = [ ];
            example = [ "api.example.com" "*.example.com" ];
            description = ''
              What the engine's sandboxes may reach besides the shared list
              (burrow.agents.allowlist and extraAllow): tinyproxy fnmatch
              patterns. No other engine's VMs get them.
            '';
          };
          proxyPort = mkOption {
            type = types.port;
            example = 3128;
            description = ''
              The host's loopback port of this engine's filtered proxy. Fixed per
              engine, so a running VM's forward never lands on another engine's
              filter after an engine is added; 3129 is the open proxy's.
            '';
          };
          capabilities = mkOption {
            type = types.listOf types.str;
            default = [ ];
            description = ''
              What the owner has approved this engine's runs may be granted
              (#743's grants). Carried as data only: nothing grants or
              enforces them yet.
            '';
          };
        };
      });
    };

    defaultEngine = mkOption {
      type = types.nullOr types.str;
      default =
        let names = lib.attrNames config.burrow.agents.engines;
        in if lib.length names == 1 then lib.head names else null;
      defaultText = lib.literalMD "the one engine, when there's exactly one";
      description = "The engine `agent new` starts a sandbox for when it's given no `--engine` (as `proj run --vm` gives none).";
    };
  };
}
