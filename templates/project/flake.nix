{
  description = "PROJECT";

  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow";

  # `nix develop` for the toolchains, `nix flake check` for the gate. Keep
  # only the languages this repo has (see burrow's lib/project.nix).
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    rust = { };
    python = { };
  };
}
