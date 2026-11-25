# Isolated Antigravity engine surface and sandboxed Conary v5 job plugin.
#
# Machine flakes supply only values (package/binary path, isolated credential
# file, egress host allowlist, runtime models and approved capabilities). This
# module declares no model IDs and no interactive desktop/T3 userdata paths,
# and wires Antigravity into burrow.agents.engines.antigravity without touching
# any other engine or burrow.proj.
inputs:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    types
    ;
  cfg = config.burrow.engines.antigravity;
  efforts = [
    "low"
    "medium"
    "high"
    "xhigh"
    "max"
  ];

  resolvedBinary =
    if cfg.binary != null then
      cfg.binary
    else if cfg.package != null then
      lib.getExe cfg.package
    else
      null;

  validCredentialHomePath =
    let
      p = cfg.credentialHomePath;
      parts = lib.splitString "/" p;
    in
    p != ""
    && !(lib.hasPrefix "/" p)
    && builtins.all (seg: seg != "" && seg != "." && seg != "..") parts;

  pluginConfig = pkgs.writeText "burrow-engine-antigravity.json" (
    builtins.toJSON ({
      binary = resolvedBinary;
      models = map (m: {
        inherit (m) id efforts;
        context_tokens = m.contextTokens;
        max_output_tokens = m.maxOutputTokens;
      }) cfg.models;
      default_model = cfg.defaultModel;
      default_effort = cfg.defaultEffort;
      credential_env = cfg.credentialEnv;
      target_credential_env = cfg.targetCredentialEnv;
      credential_mode = cfg.credentialMode;
      credential_home_path = cfg.credentialHomePath;
      settings_file = cfg.settingsFile;
      mode = cfg.mode;
      dangerously_skip_permissions = cfg.dangerouslySkipPermissions;
      sandbox = cfg.sandbox;
      disable_slash_commands = true;
      output_format = cfg.outputFormat;
      add_dirs = cfg.addDirs;
      print_timeout_secs = cfg.printTimeoutSeconds;
      deadline_secs = cfg.deadlineSeconds;
      network = lib.elem "network" cfg.capabilities;
      devices = lib.optional (lib.elem "kvm" cfg.capabilities) "kvm";
      meters = cfg.meters;
    })
  );

  r2Patterns = import ./anthropic/r2.nix;
  # Convert each Claude Bash-pattern entry in r2.nix into Antigravity CLI
  # `command(...)` deny rules (plus `sudo` variants):
  # - Trailing ` *` is a whole-token prefix (e.g. `zfs destroy *` -> `command(zfs destroy)`).
  # - Any remaining `*` wildcard inside a token (e.g. `mkfs*`, `fwupdmgr update*`,
  #   `git push --force origin main*`) uses Antigravity's `regex:` token-regex
  #   syntax (`command(regex:mkfs.*)`), because plain `command(...)` matches
  #   command prefixes word-by-word literally.
  r2ToAntigravityRules =
    pat:
    let
      trimmed = lib.removeSuffix " *" pat;
      toRegex = s: lib.concatMapStringsSep ".*" lib.escapeRegex (lib.splitString "*" s);
    in
    if lib.hasInfix "*" trimmed then
      let
        rx = toRegex trimmed;
      in
      [
        "command(regex:${rx})"
        "command(regex:sudo ${rx})"
      ]
    else
      [
        "command(${trimmed})"
        "command(sudo ${trimmed})"
      ];
  # Default headless host settings: allow workspace edits and commands without
  # interactive prompts while denying every R2 one-way command from r2.nix
  # (one fact, one home shared with the Anthropic host settings).
  defaultHostSettingsFile = pkgs.writeText "burrow-antigravity-host-settings.json" (
    builtins.toJSON {
      toolPermission = "always-proceed";
      permissions = {
        deny = lib.concatMap r2ToAntigravityRules r2Patterns;
      };
    }
  );

  hostPluginConfig = pkgs.writeText "burrow-engine-antigravity-host.json" (
    builtins.toJSON ({
      binary = resolvedBinary;
      models = map (m: {
        inherit (m) id efforts;
        context_tokens = m.contextTokens;
        max_output_tokens = m.maxOutputTokens;
      }) cfg.models;
      default_model = cfg.defaultModel;
      default_effort = cfg.defaultEffort;
      credential_env = cfg.credentialEnv;
      target_credential_env = cfg.targetCredentialEnv;
      credential_file = cfg.host.credentialFile;
      credential_mode = cfg.credentialMode;
      credential_home_path = cfg.credentialHomePath;
      settings_file =
        if cfg.host.settingsFile != null then
          cfg.host.settingsFile
        else if cfg.settingsFile != null then
          cfg.settingsFile
        else
          defaultHostSettingsFile;
      mode = cfg.host.mode;
      host_mode = true;
      dangerously_skip_permissions = false;
      sandbox = cfg.host.sandbox;
      disable_slash_commands = true;
      output_format = cfg.outputFormat;
      add_dirs = cfg.addDirs;
      print_timeout_secs = cfg.printTimeoutSeconds;
      deadline_secs = cfg.deadlineSeconds;
      network = lib.elem "network" cfg.capabilities;
      devices = lib.optional (lib.elem "kvm" cfg.capabilities) "kvm";
      meters = cfg.meters;
    })
  );

  pluginBin = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.burrow-engine-antigravity;

  wrappedPlugin =
    pkgs.runCommand "burrow-engine-antigravity-plugin"
      {
        nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
        meta.mainProgram = "burrow-engine-antigravity";
        passthru.config = pluginConfig;
      }
      ''
        makeBinaryWrapper ${lib.getExe' pluginBin "burrow-engine-antigravity"} \
          $out/bin/burrow-engine-antigravity \
          --set BURROW_ANTIGRAVITY_CONFIG ${pluginConfig}
      '';

  wrappedHostPlugin =
    pkgs.runCommand "burrow-engine-antigravity-host-plugin"
      {
        nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
        meta.mainProgram = "burrow-engine-antigravity-host";
        passthru.config = hostPluginConfig;
      }
      ''
        makeBinaryWrapper ${lib.getExe' pluginBin "burrow-engine-antigravity"} \
          $out/bin/burrow-engine-antigravity-host \
          --set BURROW_ANTIGRAVITY_CONFIG ${hostPluginConfig}
      '';
in
{
  imports = [ (import ../common.nix inputs) ];

  options.burrow.engines.antigravity = {
    enable = mkEnableOption "Antigravity engine surface and sandboxed Conary v5 job plugin";

    package = mkOption {
      type = types.nullOr types.package;
      default = null;
      description = ''
        Package providing the `antigravity` CLI binary, installed inside the
        Antigravity engine's microVM sandbox. Supply either `package` or
        `binary`.
      '';
    };

    binary = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/current-system/sw/bin/antigravity";
      description = ''
        Explicit path to the `antigravity` executable inside the sandbox. When
        null, derived from `package`.
      '';
    };

    credentialFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/antigravity-agent-credential";
      description = ''
        Host path to an isolated Antigravity auth credential (for example from
        sops-nix), strictly independent of interactive desktop or T3 userdata.
        Delivered to Antigravity sandboxes as systemd credential
        `agent.credential` and never shared with other engines.
      '';
    };

    credentialEnv = mkOption {
      type = types.str;
      default = "BURROW_ANTIGRAVITY_CREDENTIAL";
      description = ''
        Environment variable name under which the sandbox hands `credentialFile`
        to the Antigravity plugin.
      '';
    };

    targetCredentialEnv = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "ANTIGRAVITY_API_KEY";
      description = ''
        Optional environment variable name passed to the `antigravity` CLI when
        different from `credentialEnv`. When null, `credentialEnv` is used.
      '';
    };

    credentialMode = mkOption {
      type = types.enum [
        "env"
        "file"
        "home-file"
      ];
      default = "env";
      description = ''
        How to deliver the isolated credential to `antigravity`: directly as an
        environment variable (`env`), as a path to a private `0600` per-run
        file in an environment variable (`file`), or materialized at
        `$HOME/<credentialHomePath>` (`0600`, writable for OAuth refreshes)
        with credential environment variables stripped (`home-file`).
      '';
    };

    credentialHomePath = mkOption {
      type = types.str;
      default = ".gemini/antigravity-cli/antigravity-oauth-token";
      description = ''
        Relative path under `$HOME` where `credentialMode = "home-file"`
        materializes the isolated OAuth credential file (`0600`). Must be a
        non-empty relative path with only normal path components so it cannot
        escape `$HOME`.
      '';
    };

    hosts = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [
        "antigravity.example.com"
        "*.googleapis.com"
      ];
      description = ''
        Egress host allowlist (tinyproxy fnmatch patterns) for Antigravity
        sandboxes alone, wired into `burrow.agents.engines.antigravity.hosts`.
      '';
    };

    proxyPort = mkOption {
      type = types.port;
      default = 3135;
      description = ''
        Dedicated host loopback port for Antigravity's filtered egress proxy
        (`agent-proxy-antigravity`). Must not collide with 3128 (Claude), 3129
        (open), or another engine's port.
      '';
    };

    models = mkOption {
      default = [ ];
      description = ''
        Runtime-configured models advertised by Conary `describe`. Burrow
        hardcodes no model IDs; the machine flake supplies its own list.
      '';
      type = types.listOf (
        types.submodule {
          options = {
            id = mkOption {
              type = types.str;
              description = "Model identifier accepted by `antigravity --model`.";
            };
            efforts = mkOption {
              type = types.listOf (types.enum efforts);
              default = efforts;
              description = "Effort levels accepted for this model.";
            };
            contextTokens = mkOption {
              type = types.nullOr types.ints.positive;
              default = null;
              description = "Optional context window size in tokens.";
            };
            maxOutputTokens = mkOption {
              type = types.nullOr types.ints.positive;
              default = null;
              description = "Optional maximum output size in tokens.";
            };
          };
        }
      );
    };

    defaultModel = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "Default model ID when a job request leaves model unset; must appear in `models`.";
    };

    defaultEffort = mkOption {
      type = types.nullOr (types.enum efforts);
      default = null;
      description = "Default effort level when a job request leaves effort unset.";
    };

    mode = mkOption {
      type = types.nullOr (
        types.enum [
          "accept-edits"
          "auto"
          "plan"
        ]
      );
      default = "auto";
      description = "Execution mode passed via `antigravity --mode`.";
    };

    dangerouslySkipPermissions = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Pass `--dangerously-skip-permissions` to `antigravity`. Intended for
        microVM sandboxes where the VM itself is the security boundary.
      '';
    };

    sandbox = mkOption {
      type = types.bool;
      default = false;
      description = "Pass `--sandbox` to `antigravity`.";
    };

    outputFormat = mkOption {
      type = types.enum [
        "stream-json"
        "json"
        "text"
      ];
      default = "stream-json";
      description = "Output format passed via `antigravity --output-format`.";
    };

    settingsFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Optional settings file passed via `antigravity --settings`.";
    };

    addDirs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "Additional directories passed via repeatable `antigravity --add-dir`.";
    };

    printTimeoutSeconds = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      description = "Optional timeout in seconds passed via `antigravity --print-timeout`.";
    };

    deadlineSeconds = mkOption {
      type = types.nullOr types.ints.positive;
      default = null;
      description = "Optional outer hard wall-clock deadline in seconds enforced on the child process group; null leaves turn deadlines to the caller (such as proj's timeout).";
    };

    meters = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "quota" ];
      description = "Usage meter names advertised by Conary `describe`.";
    };

    capabilities = mkOption {
      type = types.listOf types.str;
      default = [
        "workspace"
        "job-state"
        "git-metadata"
        "toolchains"
        "system"
        "probe"
        "units-clear"
        "heavy"
        "nix"
        "network"
      ];
      description = ''
        Owner-approved job capabilities for `burrow.agents.engines.antigravity`.
        Also controls whether the plugin's `describe` declares `network` and
        `kvm` so declared and approved sets match. `mcp` is refused: the
        plugin doesn't hand the CLI the servers a sandbox approves.
      '';
    };

    extraPackages = mkOption {
      type = types.listOf types.package;
      default = [ ];
      description = "Additional packages installed in the Antigravity sandbox image.";
    };

    guestModules = mkOption {
      type = types.listOf types.deferredModule;
      default = [ ];
      description = "Extra NixOS modules for the Antigravity sandbox guest image.";
    };

    host = {
      enable = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Also wire a host-mode Antigravity plugin (`Sandbox::None`,
          `--dangerously-skip-permissions` refused, isolated per-turn `$HOME`)
          into `burrow.proj` so host `proj` roles can run on Antigravity while
          `--vm` jobs keep using the strict sandbox plugin.
        '';
      };

      credentialFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/run/secrets/antigravity-host-credential";
        description = ''
          Dedicated isolated credential file readable by the host `proj` user
          for host-mode Antigravity turns. Must be distinct from
          `credentialFile` so host jobs never share a credential (or root-only
          VM/judge secret) with sandboxed VMs.
        '';
      };

      mode = mkOption {
        type = types.nullOr (
          types.enum [
            "accept-edits"
            "auto"
            "plan"
          ]
        );
        default = "accept-edits";
        description = ''
          Execution mode passed via `antigravity --mode` on host turns.
          Defaults to `accept-edits` because `auto` is unavailable on business
          and enterprise accounts (falling back to `request-review`).
        '';
      };

      sandbox = mkOption {
        type = types.bool;
        default = false;
        description = "Pass `--sandbox` to `antigravity` on host turns.";
      };

      settingsFile = mkOption {
        type = types.nullOr types.path;
        default = null;
        description = "Optional settings file passed via `antigravity --settings` on host turns (defaults to `settingsFile`).";
      };
    };

    pluginPackage = mkOption {
      type = types.package;
      readOnly = true;
      default = wrappedPlugin;
      description = "The configured sandbox `burrow-engine-antigravity` Conary v5 plugin executable.";
    };

    hostPluginPackage = mkOption {
      type = types.package;
      readOnly = true;
      default = wrappedHostPlugin;
      description = "The configured host-mode `burrow-engine-antigravity` Conary v5 plugin executable.";
    };
  };

  config = mkIf cfg.enable {
    assertions = [
      {
        assertion = resolvedBinary != null;
        message = "burrow.engines.antigravity: set `package` or `binary` when enabled.";
      }
      {
        assertion = cfg.defaultModel == null || builtins.any (m: m.id == cfg.defaultModel) cfg.models;
        message = "burrow.engines.antigravity.defaultModel (${toString cfg.defaultModel}) must appear in burrow.engines.antigravity.models.";
      }
      {
        assertion =
          cfg.defaultModel == null
          || cfg.defaultEffort == null
          || builtins.any (m: m.id == cfg.defaultModel && lib.elem cfg.defaultEffort m.efforts) cfg.models;
        message = "burrow.engines.antigravity.defaultEffort (${toString cfg.defaultEffort}) must be offered by defaultModel (${toString cfg.defaultModel}).";
      }
      {
        assertion = !(lib.elem "mcp" cfg.capabilities);
        message = "burrow.engines.antigravity: `mcp` can't be in capabilities: the plugin doesn't hand the antigravity CLI the servers a sandbox approves.";
      }
      {
        assertion = validCredentialHomePath;
        message = "burrow.engines.antigravity.credentialHomePath (${cfg.credentialHomePath}) must be a non-empty relative path with only normal components (no leading `/`, `.`, `..`, or empty segments).";
      }
      {
        assertion =
          !cfg.host.enable
          || cfg.credentialFile == null
          || (cfg.host.credentialFile != null && cfg.host.credentialFile != cfg.credentialFile);
        message = "burrow.engines.antigravity.host.credentialFile must be set and distinct from burrow.engines.antigravity.credentialFile so host and VM turns do not share a credential.";
      }
      {
        assertion =
          !cfg.host.enable || cfg.credentialMode != "home-file" || cfg.host.credentialFile != null;
        message = "burrow.engines.antigravity.host.credentialFile must be set when burrow.engines.antigravity.host.enable is true and credentialMode is \"home-file\".";
      }
    ];

    burrow.agents.engines.antigravity = {
      packages = lib.optional (cfg.package != null) cfg.package ++ cfg.extraPackages;
      inherit (cfg)
        guestModules
        credentialFile
        credentialEnv
        hosts
        proxyPort
        capabilities
        ;
      plugin = wrappedPlugin;
    };
  };
}
