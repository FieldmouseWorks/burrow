# Anthropic engine surface: host Claude Code configuration, the PreToolUse
# guard adapter into burrow-guard, and the sandbox / plugin entries for both
# Anthropic modes (`claude` subscription CLI and `claude-api` credit mode).
#
# On the host, Claude Code runs system-wide in bypassPermissions; the only
# prompts are the one-way doors (R2) in anthropic/r2.nix. Standing-authority
# commands in anthropic/standing.nix are allowed outright for sessions whose
# mode puts an auto-mode check in front of the rest. Also writes
# /etc/claude-code/CLAUDE.md from what each burrow module adds to
# burrow.claude.machineMd.
#
# In the shared sandbox (burrow.agents.engines), this module wires both
# `claude` and `claude-api` with their package, guest settings, credential
# variables, Anthropic egress hosts, and packaged burrow-engine-anthropic
# plugin executables (agent-plugin-claude and agent-plugin-claude-api).
inputs:
{
  pkgs,
  lib,
  config,
  ...
}:
let
  cfg = config.burrow.claude;
  inherit (config.networking) hostName;

  # The PreToolUse hook adapter into burrow-guard, Claude Code's.
  guard = import ./anthropic/guard.nix { inherit inputs pkgs lib config; };

  # One rule per command, with and without sudo.
  both = cmds: lib.concatMap (c: [ "Bash(${c})" "Bash(sudo ${c})" ]) cmds;

  r2 = both ((import ./anthropic/r2.nix) ++ cfg.extraAsk);
  standing = map (c: "Bash(${c})") (import ./anthropic/standing.nix);

  managed = lib.recursiveUpdate (
    {
      permissions = {
        defaultMode = "bypassPermissions";
        ask = r2;
        allow = standing;
      };
      skipDangerousModePermissionPrompt = true;
      env = {
        # Updates arrive through the machine's flake, never in place.
        DISABLE_AUTOUPDATER = "1";
      };
      hooks.PreToolUse = [
        {
          matcher = "Bash";
          hooks = [
            {
              type = "command";
              command = lib.getExe guard.claude;
              inherit (guard) timeout;
            }
          ];
        }
      ];
    }
    // lib.optionalAttrs (cfg.effort != null) {
      effortLevel = cfg.effort;
    }
  ) cfg.extraSettings;

  robot = config.burrow.robot.enable or false;

  # The ways work gets done here, cheapest first (docs/front-door.md), as far
  # as this machine has them. Each door's own module says how to use it.
  doors =
    lib.optional (config.burrow.probe.enable or false) ''
      A probe, for a fact that only needs reading (memory, a unit's log, what's deployed,
         the queue): `burrow-probe ask QUESTION` here, or a `${config.burrow.probe.label}` issue in ${config.burrow.probe.repo}.''
    ++ lib.optional (config.burrow.autoland.enable or false) ''
      A run, for a build, gate or proof whose exit is its verdict: a repo's `proofBuilds` or
         `proofRuns` (burrow.autoland.repos), which the steward runs on each settled PR's head
         with no Claude. Never a job, or a `proofJobs` prompt, that only runs one command.''
    ++ lib.optional (config.burrow.proj.enable or false) ''
      A job, for judgment in a project: `proj run`${lib.optionalString (config.burrow.proj.inbox.enable or false) ", or an issue labelled `${lib.concatStringsSep "`, `" (lib.attrNames config.burrow.proj.inbox.labels)}`"}.'';

  anthropicHosts = [
    "api.anthropic.com"
    "*.anthropic.com"
    "claude.ai"
    "*.claude.ai"
    "*.claude.com"
  ];

  guestModule = import ./anthropic/guest.nix { inherit (cfg) effort; };

  # Resolve defaultEffort for a mode's plugin config: an explicit override is
  # passed verbatim (so PluginConfig::validate catches mistakes); otherwise
  # inherit burrow.claude.effort only when models are configured and the
  # default model accepts it (conary-plugin v5 forbids effort without a model).
  resolveDefaultEffort =
    {
      models,
      defaultModel,
      defaultEffort,
    }:
    let
      modelId =
        if defaultModel != null then
          defaultModel
        else if models != [ ] then
          (builtins.head models).id
        else
          null;
      modelObj =
        if modelId == null then null else lib.findFirst (m: m.id == modelId) null models;
    in
    if defaultEffort != null then
      defaultEffort
    else if cfg.effort != null && modelObj != null && lib.elem cfg.effort modelObj.efforts then
      cfg.effort
    else
      null;

  mkAnthropicPlugin =
    {
      mode,
      credentialEnv,
      models,
      defaultModel,
      defaultEffort,
      meters,
      apiBaseUrl ? null,
      maxBudgetUsd ? null,
    }:
    import ./anthropic/plugin.nix {
      inherit (inputs) self;
      inherit
        pkgs
        lib
        mode
        credentialEnv
        models
        defaultModel
        meters
        apiBaseUrl
        maxBudgetUsd
        ;
      program = lib.getExe' cfg.package "claude";
      defaultEffort = resolveDefaultEffort { inherit models defaultModel defaultEffort; };
      capabilities = {
        inherit (cfg.plugin.capabilities) network mcp devices;
      };
      inherit (cfg.plugin) appendSystemPrompt;
    };

  apiModels = if cfg.api.models != null then cfg.api.models else cfg.plugin.models;
  apiDefaultModel =
    if cfg.api.defaultModel != null then cfg.api.defaultModel else cfg.plugin.defaultModel;
  apiDefaultEffort =
    if cfg.api.defaultEffort != null then cfg.api.defaultEffort else cfg.plugin.defaultEffort;
  apiKeyFile =
    if cfg.api.keyFile != null then
      cfg.api.keyFile
    else
      config.burrow.proj.claudeApi.keyFile or null;

  modelType = lib.types.submodule {
    options = {
      id = lib.mkOption {
        type = lib.types.str;
        description = "Model identifier advertised by the Anthropic plugin's describe.";
      };
      efforts = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        description = "Effort levels this model accepts.";
      };
      contextTokens = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = "Context window in tokens, if declared.";
      };
      maxOutputTokens = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = "Maximum output tokens, if declared.";
      };
    };
  };
in
{
  imports = [
    (import ../common.nix inputs)
    ./anthropic/etc.nix
  ];

  options.burrow.claude = {
    enable = lib.mkEnableOption "system-wide Claude Code with reversibility-classed permissions and Anthropic sandbox plugins";

    extraAsk = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "terraform destroy *" ];
      description = "More one-way-door commands that should ask (each also with sudo).";
    };

    extraSettings = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = "Merged into /etc/claude-code/managed-settings.json.";
    };

    boxNotes = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        Extra bullets for "How the box works" in the machine CLAUDE.md, before
        the ones burrow writes (markdown, continuation lines indented by two).
      '';
    };

    standingAuthority = lib.mkOption {
      type = lib.types.lines;
      default = "";
      description = ''
        What the owner has approved in advance, as markdown for its own section
        of the machine CLAUDE.md ("Standing authority", just before "What still
        asks"), so no session asks them to approve it again. Empty: no section.
      '';
    };

    plugin = {
      models = lib.mkOption {
        type = lib.types.listOf modelType;
        default = [ ];
        description = ''
          Models and effort levels advertised by `burrow-engine-anthropic`'s
          `describe` (runtime machine configuration; never hardcode model IDs
          in burrow).
        '';
      };

      defaultModel = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Default model ID when a job request does not select one (must appear in `models`).";
      };

      defaultEffort = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          Default effort level for plugin jobs. When null, inherits
          `burrow.claude.effort` if `models` is configured and the default model
          accepts it.
        '';
      };

      capabilities = {
        network = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether the Anthropic plugin declares `network` in `describe`.";
        };
        mcp = lib.mkOption {
          type = lib.types.bool;
          default = true;
          description = "Whether the Anthropic plugin declares `mcp` in `describe`.";
        };
        devices = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
          description = "Devices declared by the Anthropic plugin in `describe` (e.g. `[\"kvm\"]`).";
        };
      };

      meters = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf lib.types.str);
        default = null;
        description = "Override the usage meter names `claude`'s plugin advertises in `describe`.";
      };

      appendSystemPrompt = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional extra system prompt appended to plugin job invocations.";
      };
    };

    api = {
      keyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/anthropic-api-key";
        description = ''
          Path on the host holding the Anthropic API key for `claude-api`
          sandboxes and plugin calls (handed in as `agent.credential` and
          exported as `ANTHROPIC_API_KEY`). Falls back to
          `burrow.proj.claudeApi.keyFile` when set.
        '';
      };

      resolvedKeyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        readOnly = true;
        internal = true;
        default = apiKeyFile;
        description = ''
          The key file `claude-api` uses, `keyFile` or its fallback: the one
          place it's resolved, for the sandbox, the host plugin and proj's
          route readiness alike.
        '';
      };

      baseUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional `ANTHROPIC_BASE_URL` override for `claude-api` plugin runs.";
      };

      maxBudgetUsd = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = null;
        description = "Optional per-run spend cap in micro-dollars for `claude-api` plugin runs.";
      };

      models = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf modelType);
        default = null;
        description = "Models advertised by `claude-api`'s plugin. Null inherits `burrow.claude.plugin.models`.";
      };

      defaultModel = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Default model ID for `claude-api`'s plugin. Null inherits `burrow.claude.plugin.defaultModel`.";
      };

      defaultEffort = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Default effort for `claude-api`'s plugin. Null inherits `burrow.claude.plugin.defaultEffort`.";
      };

      meters = lib.mkOption {
        type = lib.types.nullOr (lib.types.listOf lib.types.str);
        default = null;
        description = "Override the usage meter names `claude-api`'s plugin advertises in `describe`.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    burrow.tools.claude-guard = guard.claude;

    # Anthropic's sandboxes and plugins (agents.nix): one entry per mode
    # (`claude` subscription CLI and `claude-api` credit mode). Each gets the
    # same Claude Code build and guest effort, its own login variable
    # (`CLAUDE_CODE_OAUTH_TOKEN` / `ANTHROPIC_API_KEY`), Anthropic's egress
    # hosts, and the packaged burrow-engine-anthropic plugin executable.
    burrow.agents.engines = {
      claude = {
        packages = [ cfg.package ];
        guestModules = [ guestModule ];
        credentialEnv = lib.mkDefault "CLAUDE_CODE_OAUTH_TOKEN";
        hosts = anthropicHosts;
        proxyPort = lib.mkDefault 3128;
        plugin = lib.mkDefault (mkAnthropicPlugin {
          mode = "claude";
          credentialEnv = config.burrow.agents.engines.claude.credentialEnv;
          inherit (cfg.plugin) models defaultModel defaultEffort meters;
        });
      };
      claude-api = {
        packages = [ cfg.package ];
        guestModules = [ guestModule ];
        credentialFile = lib.mkIf (apiKeyFile != null) (lib.mkDefault apiKeyFile);
        credentialEnv = lib.mkDefault "ANTHROPIC_API_KEY";
        hosts = anthropicHosts;
        proxyPort = lib.mkDefault 3136;
        plugin = lib.mkDefault (mkAnthropicPlugin {
          mode = "claude-api";
          credentialEnv = config.burrow.agents.engines.claude-api.credentialEnv;
          models = apiModels;
          defaultModel = apiDefaultModel;
          defaultEffort = apiDefaultEffort;
          inherit (cfg.api) meters maxBudgetUsd;
          apiBaseUrl = cfg.api.baseUrl;
        });
      };
    };
    burrow.agents.defaultEngine = lib.mkDefault "claude";

    # Copies, not links into the store (anthropic/etc.nix).
    environment.etc."claude-code/managed-settings.json" = {
      mode = "0444";
      text = builtins.toJSON managed;
    };
    environment.etc."claude-code/CLAUDE.md" = {
      mode = "0444";
      text = cfg.machineMd + "\n" + cfg.codeMd;
    };

    burrow.claude.codeMd = ''
      You have sudo with no password in Claude Code. Use it when you need it.
    '';

    # Logins and history live in ~/.claude.

    burrow.claude.machineMd = lib.mkMerge [
      (lib.mkOrder 600 (
        ''
          ## How the box works

        ''
        + lib.concatMapStrings (n: "${n}\n") (
          cfg.boxNotes
          ++ [
            ''
              - You are `${config.burrow.user}`.''
            ''
              - Projects live in ${config.burrow.projectsDir}.''
          ]
        )
      ))

      (lib.mkIf (doors != [ ]) (
        lib.mkOrder 650 ''
          ## The cheapest door first

          When you hand work on, or need something checked, take the first of these that fits. A
          probe and a run start no Claude; a job and a session each hold one, and its memory, for as
          long as it works (burrow's docs/front-door.md):

          ${lib.concatStrings (lib.imap1 (i: door: "${toString i}. ${door}\n") doors)}''
      ))

      (lib.mkOrder 1000 ''
        ## When a tool gets in the way: patch it

        ${hostName} is built from source, so fix the tool before working around it. When an
        open-source piece of the stack misbehaves for agents (prompts, needs a TTY,
        bad defaults, unparseable output), patch it with a Nix overlay in the flake
        (`nixpkgs.overlays`, patch files under `patches/`), switch, and send the patch
        upstream when it's useful beyond ${hostName}. Don't build a wrapper, shim or
        retry loop around something you could fix.

        Claude Code is the exception: it's closed source, so it gets wrapped and
        configured (burrow's modules/engines/anthropic.nix), not patched.
      '')

      (lib.mkIf (cfg.standingAuthority != "") (
        lib.mkOrder 1150 ''
          ## Standing authority

          ${cfg.standingAuthority}''
      ))

      (lib.mkOrder 1200 ''
        ## What still asks ${config.burrow.owner}

        Only one-way doors: destroying or rolling back ZFS datasets, wiping or
        partitioning disks, deleting remote data (rclone delete/purge/sync, gh repo
        delete), force-pushing a default branch, flashing firmware${lib.optionalString robot ", and Hetzner Robot\ncalls that cancel or reinstall the server"}. Everything else: just do it, then
        say what you did.${lib.optionalString (cfg.standingAuthority != "") "\nThe standing authority above is already given: never ask ${config.burrow.owner} to restate it."}
      '')
    ];
  };
}
