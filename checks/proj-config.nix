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
    excludesMachinery = authors:
      builtins.all (author: author.id != inbox.config.burrow.github.machineApp.accountId) authors;
    injected = inbox.config.burrow.proj.inbox.authors ++ [ {
      login = "renamed-machinery";
      id = inbox.config.burrow.github.machineApp.accountId;
    } ];
  in
  assert inbox.config.systemd.services.burrow-engines-door.serviceConfig.User == inbox.config.burrow.user;
  assert inbox.config.burrow.events.handlers.engines.events == [ "issues" ];
  assert inbox.config.burrow.events.handlers.engines.repos == [ "sam/homelab" ];
  assert builtins.elem "proj-config" inbox.config.burrow.updates.requiredHealth;
  assert builtins.elem "switch-engines" inbox.config.burrow.proj.inbox.triage.skip;
  assert builtins.elem "custom" inbox.config.burrow.proj.inbox.triage.skip;
  assert inbox.config.burrow.proj.inbox.planning;
  assert excludesMachinery inbox.config.burrow.proj.inbox.authors;
  assert !(excludesMachinery injected);
  assert builtins.elem "pull_request" inbox.config.burrow.events.handlers.proj-inbox.events;
  # An issue's event tells the inbox's pass that issue alone, in burrow-events'
  # runtime directory, under the unit's instance.
  assert inbox.config.burrow.events.handlers.proj-inbox.issueNumbers;
  assert inbox.config.systemd.services."proj-inbox@".environment.BURROW_EVENTS_ISSUES == "/run/burrow-events/proj-inbox/%i";
  assert inbox.config.systemd.services.burrow-events.serviceConfig.RuntimeDirectory == "burrow-events";
  pkgs.runCommand "proj-config-check" { } ''
    got=$(${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj state no-such-job)
    [ "$got" = failed ] || { echo "proj state said: $got"; exit 1; }
    got=$(${inbox.config.burrow.tools.proj-inbox}/bin/proj-inbox status)
    [ -z "$got" ] || { echo "proj-inbox status said: $got"; exit 1; }
    # check-config also holds the sandbox controller proj pauses and
    # releases VM jobs with to a program it can run (#711).
    ${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj check-config
    touch $out
  ''
