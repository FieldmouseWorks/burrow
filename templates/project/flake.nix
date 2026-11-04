{
  description = "PROJECT";

  # Pinned to a commit of burrow's main, so Renovate moves it with the pins
  # in .github/workflows, in one PR (burrow's
  # modules/updates/renovate-policy.nix says when); `nix flake update`
  # leaves it where it is.
  inputs.burrow.url = "git+https://github.com/FieldmouseWorks/burrow?ref=main&rev=1ba23109c9d5866e654db3aff06cde6a8cbc1a9f";

  # `nix develop` for the toolchains, `nix flake check` for the gate. Keep
  # only the languages this repo has (see burrow's lib/project.nix).
  outputs = { self, burrow }: burrow.lib.project {
    inherit self;
    rust = { };
    python = { };
  };
}
