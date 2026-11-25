# The example's proj reads the config its module writes (a field the
# binary doesn't know fails it), and a job it doesn't have is failed;
# so does its proj-inbox, with the inbox on, which has no issues yet.
{ self, pkgs, ... }:
let
  inbox = self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.proj.inbox = {
          enable = true;
          authors = [
            {
              login = "octocat";
              id = 1;
            }
          ];
        };
        burrow.proj.engines.door = {
          enable = true;
          repo = "sam/homelab";
          label = "switch-engines";
        };
        burrow.proj.inbox.triage.skip = [ "custom" ];
      }
    ];
  };
  excludesMachinery =
    authors:
    builtins.all (author: author.id != inbox.config.burrow.github.machineApp.accountId) authors;
  # A key set only as burrow.claude.api.keyFile is the one route readiness
  # reads (proj.json's claudeApiKeyFile), as the host plugin is given it.
  # A sandbox-only plugin wired in burrow.agents.engines is recorded in
  # proj.json's sandboxPlugins so host turns refuse it without running it.
  # An extra host plugin in burrow.proj.plugins is verified at build time
  # in a pure sandbox to prove it does not declare Sandbox::Needs.
  hostExtraPlugin = pkgs.writeShellScriptBin "extra-host-plugin" ''
    cat >/dev/null
    printf '{"contract":"conary-plugin/v1","by":{"engine":"extra-host"},"versions":["conary-plugin/v5"],"v2_kinds":["job"],"models":[],"capabilities":{"sandbox":"none","network":true,"devices":[],"mcp":false},"meters":[]}\n'
  '';
  sandboxExtraPlugin = pkgs.writeShellScriptBin "extra-vm-plugin" ''
    cat >/dev/null
    printf '{"contract":"conary-plugin/v1","by":{"engine":"extra-vm"},"versions":["conary-plugin/v5"],"v2_kinds":["job"],"models":[],"capabilities":{"sandbox":"needs","network":true,"devices":[],"mcp":false},"meters":[]}\n'
  '';
  keyed = self.nixosConfigurations.example.extendModules {
    modules = [
      {
        burrow.claude.api.keyFile = "/run/keys/anthropic-api";
        burrow.proj.plugins.extra-host = hostExtraPlugin;
        burrow.agents.engines.sandboxed = {
          proxyPort = 3199;
          plugin = pkgs.hello;
          capabilities = [ "workspace" ];
        };
        burrow.engines.antigravity = {
          enable = true;
          package = pkgs.hello;
          credentialFile = "/run/keys/antigravity-oauth";
          credentialMode = "home-file";
          host = {
            enable = true;
            credentialFile = "/run/keys/antigravity-host-oauth";
          };
        };
      }
    ];
  };
  # burrow.proj.plugins only adds engines: naming a wired one (claude-api,
  # whose wrapper holds the credit and its key) fails an assertion.
  replaced = self.nixosConfigurations.example.extendModules {
    modules = [ { burrow.proj.plugins.claude-api = pkgs.hello; } ];
  };
  refused = builtins.filter (a: !a.assertion) replaced.config.assertions;
  injected = inbox.config.burrow.proj.inbox.authors ++ [
    {
      login = "renamed-machinery";
      id = inbox.config.burrow.github.machineApp.accountId;
    }
  ];
in
assert builtins.any (
  a: pkgs.lib.hasInfix "burrow.proj.plugins adds engines; claude-api" a.message
) refused;
assert
  inbox.config.systemd.services.burrow-engines-door.serviceConfig.User == inbox.config.burrow.user;
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
assert
  inbox.config.systemd.services."proj-inbox@".environment.BURROW_EVENTS_ISSUES
  == "/run/burrow-events/proj-inbox/%i";
assert
  inbox.config.systemd.services.burrow-events.serviceConfig.RuntimeDirectory == "burrow-events";
# A turn's end kicks the drain's own pass (inbox/drain.rs, DRAIN_KICK):
# one unit, no repo's, that lists nothing.
assert inbox.config.burrow.events.handlers.proj-inbox-drain.unit == "proj-inbox-drain";
assert !inbox.config.burrow.events.handlers.proj-inbox-drain.perRepo;
assert pkgs.lib.hasSuffix "/proj-inbox drain"
  inbox.config.systemd.services.proj-inbox-drain.serviceConfig.ExecStart;
pkgs.runCommand "proj-config-check" { nativeBuildInputs = [ pkgs.jq ]; } ''
  conf=$(sed -n 's/.*export PROJ_CONFIG=\([^ ]*\) .*/\1/p' ${keyed.config.burrow.tools.proj}/bin/proj | head -n 1)
  jq -e '.claudeApiKeyFile == "/run/keys/anthropic-api"' "$conf" >/dev/null \
    || { echo "proj.json's claudeApiKeyFile isn't the resolved key: $(jq .claudeApiKeyFile "$conf")"; exit 1; }
  jq -e '.sandboxPlugins.sandboxed == ["workspace"] and (.sandboxPlugins | has("claude") | not) and (.sandboxPlugins | has("antigravity") | not)' "$conf" >/dev/null \
    || { echo "proj.json's sandboxPlugins missed sandboxed capabilities or included host-capable engines: $(jq .sandboxPlugins "$conf")"; exit 1; }
  jq -e '.plugins["extra-host"] | endswith("/bin/extra-host")' "$conf" >/dev/null \
    || { echo "proj.json missed verified extra-host plugin: $(jq .plugins "$conf")"; exit 1; }
  if printf '{"contract":"conary-plugin/v5","kind":"describe"}\n' | ${sandboxExtraPlugin}/bin/extra-vm-plugin | jq -e '(.capabilities.sandbox // "none") != "needs"' >/dev/null; then
    echo "extra-vm-plugin unexpectedly passed host-plugin sandbox check"
    exit 1
  fi
  jq -e '(.plugins.antigravity | endswith("/bin/burrow-engine-antigravity-host")) and (.vmPlugins.antigravity.plugin | endswith("/bin/burrow-engine-antigravity")) and (.vmPlugins.antigravity.capabilities | index("workspace") != null)' "$conf" >/dev/null \
    || { echo "proj.json did not wire dual-mode antigravity host + vm plugins cleanly: $(jq '{plugins, vmPlugins}' "$conf")"; exit 1; }
  got=$(${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj state no-such-job)
  [ "$got" = failed ] || { echo "proj state said: $got"; exit 1; }
  got=$(${inbox.config.burrow.tools.proj-inbox}/bin/proj-inbox status)
  [ -z "$got" ] || { echo "proj-inbox status said: $got"; exit 1; }
  # check-config also holds the sandbox controller proj pauses and
  # releases VM jobs with to a program it can run (#711).
  ${self.nixosConfigurations.example.config.burrow.tools.proj}/bin/proj check-config
  touch $out
''
