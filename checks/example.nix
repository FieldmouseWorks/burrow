# The example, built, and runnable as each module on its own is.
{ self, runnable, ... }:
(runnable "example" self.nixosConfigurations.example).config.system.build.toplevel
