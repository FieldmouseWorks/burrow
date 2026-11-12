# Rolling baselines and anomalies for the stall watchdog (crates/burrow-watchdog,
# anomaly.rs). Sources that already observe a number drop it in an input file;
# a pass keeps bounded history, and a sustained breach of a rule is an item in
# a watch file of its own, so it takes the watchdog's one notice lifecycle.
# Nothing here names a machine: rules and inputs are the machine's to set.
{ config, lib, ... }:
let
  cfg = config.burrow.watchdog.anomalies;
  inherit (lib) mkOption types;
  file = "${config.burrow.watchdog.dir}/anomalies-watch.json";
in
{
  options.burrow.watchdog.anomalies = {
    enable = lib.mkEnableOption "rolling baselines and anomaly notices through the stall watchdog";
    inputs = mkOption {
      type = types.listOf types.str;
      default = [ ];
      example = [ "/var/lib/burrow/example/observations.json" ];
      description = ''
        Files sources rewrite with `{"observations": [{"check", "subject",
        "value", "at", "link"}]}` (`at` in epoch seconds). A missing or
        unreadable file is missing data and changes nothing.
      '';
    };
    retention = {
      samples = mkOption {
        type = types.ints.positive;
        default = 200;
        description = "The most samples kept for each check and subject.";
      };
      days = mkOption {
        type = types.ints.positive;
        default = 14;
        description = "How long a sample is kept.";
      };
      links = mkOption {
        type = types.ints.positive;
        default = 3;
        description = "The most source links an anomaly's evidence carries.";
      };
    };
    rules = mkOption {
      default = { };
      description = "Rules by check name; observations of other checks are ignored.";
      example = lib.literalExpression ''{ latency = { mode = "ratio"; threshold = 3.0; }; }'';
      type = types.attrsOf (types.submodule {
        options = {
          mode = mkOption {
            type = types.enum [ "ratio" "above" "below" ];
            default = "ratio";
            description = ''
              `ratio`: at least `threshold` times the baseline (the median of
              the healthy samples kept); `above` or `below`: the value itself
              at or past `threshold`.
            '';
          };
          threshold = mkOption {
            type = types.float;
            description = "What a sample must reach to breach the rule.";
          };
          minSamples = mkOption {
            type = types.ints.positive;
            default = 10;
            description = "Healthy samples needed before a new one is judged (a cold start is never an anomaly).";
          };
          minBreachMinutes = mkOption {
            type = types.ints.unsigned;
            default = 30;
            description = "How long a breach must hold to be an anomaly; a shorter one is transient.";
          };
          clearMinutes = mkOption {
            type = types.ints.unsigned;
            default = 30;
            description = "How long healthy samples must hold to clear an anomaly.";
          };
        };
      });
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [{
      assertion = config.burrow.watchdog.enable && cfg.rules != { };
      message = "burrow.watchdog.anomalies needs watchdog.enable and at least one rule.";
    }];
    burrow.watchdog.sources.anomalies = {
      inherit file;
      about = "The anomaly detector over the numbers the watchdog's sources observe";
      look = "Read the item's evidence (observed value, baseline, window, links) and the check's source. `burrow-watchdog dismiss CHECK SUBJECT` holds a false alarm out until it clears.";
      kick = null;
      staleMinutes = null;
    };
    # The module writes the section the watchdog's config carries.
    burrow.watchdog.anomaliesConfig = {
      inputs = cfg.inputs;
      inherit file;
      retentionSamples = cfg.retention.samples;
      retentionSeconds = cfg.retention.days * 86400;
      maxLinks = cfg.retention.links;
      rules = lib.mapAttrs (_: r: {
        inherit (r) mode threshold minSamples;
        minBreachSeconds = r.minBreachMinutes * 60;
        clearSeconds = r.clearMinutes * 60;
      }) cfg.rules;
    };
  };
}
