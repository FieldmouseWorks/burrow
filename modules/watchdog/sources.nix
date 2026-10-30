# The watchdog's options its sources use, on their own: a module with waits
# to watch (autoland, proj) adds its source here without importing the
# watchdog, which imports proj.
{ config, lib, ... }:
let
  inherit (lib) mkOption types;
in
{
  options.burrow.watchdog = {
    dir = mkOption {
      type = types.str;
      default = "${config.burrow.stateDir}/watchdog";
      defaultText = lib.literalExpression ''"''${config.burrow.stateDir}/watchdog"'';
      readOnly = true;
      description = ''
        Its state. escalated.json there lists the stalls it took past a Claude
        job, as `[why, line, desc]` digest entries, for the steward to put on
        its stuck issue (autoland/waits.py).
      '';
    };

    sources = mkOption {
      default = { };
      description = ''
        What it watches, by name: each a JSON file its source rewrites with
        what it waits on now, `{"at": EPOCH, "items": {KEY: {"what", "link",
        "phase", "since", "due", "kick"}}}` (crates/burrow-watchdog). The
        modules that have such waits add theirs.
      '';
      type = types.attrsOf (types.submodule {
        options = {
          file = mkOption {
            type = types.str;
            description = "The source's watch file.";
          };
          about = mkOption {
            type = types.str;
            description = "The source, in words, for when it's stuck itself.";
          };
          look = mkOption {
            type = types.str;
            description = "Where a Claude job finds out why one of its items is stuck, in a sentence.";
          };
          kick = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "The burrow-events handler to kick when the source itself is stuck.";
          };
          staleMinutes = mkOption {
            type = types.nullOr types.ints.positive;
            default = null;
            description = ''
              The source is stuck itself once its file is this much older
              than its last write should be (it writes on every pass, say,
              and has a pass at least hourly). null: it writes only when
              something changes, so an old file is no news.
            '';
          };
        };
      });
    };
  };
}
