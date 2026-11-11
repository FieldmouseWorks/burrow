# The example's proj reads the config its module writes (a field the
# binary doesn't know fails it), and a job it doesn't have is failed;
# so does its proj-inbox, with the inbox on, which has no issues yet.
{ self, pkgs, ... }:
  let
    inbox = self.nixosConfigurations.example.extendModules {
      modules = [ {
        burrow.proj.inbox = { enable = true; authors = [ { login = "octocat"; id = 1; } ]; };
        burrow.proj.engines.door = { enable = true; repo = "sam/homelab"; label = "switch-engines"; };
        burrow.proj.inbox.triage.skip = [ "custom" ];
      } ];
    };
  in
  assert inbox.config.systemd.services.burrow-engines-door.serviceConfig.User == inbox.config.burrow.user;
  assert inbox.config.burrow.events.handlers.engines.events == [ "issues" ];
  assert inbox.config.burrow.events.handlers.engines.repos == [ "sam/homelab" ];
  assert builtins.elem "proj-config" inbox.config.burrow.updates.requiredHealth;
  assert builtins.elem "switch-engines" inbox.config.burrow.proj.inbox.triage.skip;
  assert builtins.elem "custom" inbox.config.burrow.proj.inbox.triage.skip;
  pkgs.runCommand "proj-config-check" { } ''
    got=$(${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj state no-such-job)
    [ "$got" = failed ] || { echo "proj state said: $got"; exit 1; }
    got=$(${inbox.config.burrow.tools.proj-inbox}/bin/proj-inbox status)
    [ -z "$got" ] || { echo "proj-inbox status said: $got"; exit 1; }
    ${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj check-config
    touch $out
  ''
