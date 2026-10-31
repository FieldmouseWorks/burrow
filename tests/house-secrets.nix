# lib/project.nix's secrets check, on a project that leaks: it fails on a
# token in any tracked text file, the ones gitleaks' own config skips
# (lockfiles, SVGs, vendored and minified code) included, it keeps the repo's
# own answers to a false alarm (`gitleaks:allow`, .gitleaksignore), and its
# log says where each finding is and never what it is: no token, no part of
# one, not the password beside one, a private key's body or a token in
# base64. The secrets are put together here, so no tracked file holds one
# (GitHub's push protection would turn the push away, and burrow's own
# secrets check would fail).
#
#   nix build .#checks.x86_64-linux.house-secrets-leak -L
{ self, pkgs }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  hex = seed: builtins.hashString "sha256" "house-secrets-leak ${seed}";
  # A GitHub classic token: ghp_ and 36 letters and digits.
  token = seed: "gh" + "p_" + builtins.substring 0 36 (hex seed);
  # No rule knows it, and it sits on a line with two tokens.
  password = "pw-" + builtins.substring 0 24 (hex "password");
  keyLines = map (n: builtins.substring 0 64 (hex "key ${toString n}")) [ 1 2 3 ];
  key = "-----BEGIN RSA " + "PRIVATE KEY-----\n" + lib.concatMapStrings (l: l + "\n") keyLines
    + "-----END RSA " + "PRIVATE KEY-----";
  encode = "echo -n '${token "encoded"}' | base64 -w0";
  # Where gitleaks' own config looks away, one token each.
  skipped = [
    "package-lock.json"
    "pnpm-lock.yaml"
    "go.sum"
    "gradlew"
    "logo.svg"
    "jquery-3.7.1.min.js"
    "vendor/github.com/acme/lib/lib.go"
    "node_modules/left-pad/index.js"
    "node_modules/@octokit/auth-token/README.md"
  ];
  leaky = pkgs.runCommand "leaky-project" { } ''
    mkdir $out && cd $out
    echo 'password = "${password}" token = "${token "a"}" backup = "${token "b"}"' > config.toml
    cat > deploy-key.pem <<'EOF'
    ${key}
    EOF
    echo "auth = \"$(${encode})\"" > encoded.txt
    ${lib.concatMapStrings (f: ''
      mkdir -p "$(dirname '${f}')"
      echo 'url = "https://ci:${token f}@example.com/"' > '${f}'
    '') skipped}
    echo 'token = "${token "allowed"}" # gitleaks:allow' > allowed.txt
    echo 'token = "${token "ignored"}"' > ignored.txt
    echo 'ignored.txt:github-pat:1' > .gitleaksignore
  '';
  secrets = [ password (token "a") (token "b") (token "encoded") (token "allowed") (token "ignored") ]
    ++ keyLines ++ map token skipped;
  failed = pkgs.testers.testBuildFailure
    (self.lib.project { self = leaky; systems = [ system ]; }).checks.${system}.secrets;
in
pkgs.runCommand "house-secrets-leak-check" { } ''
  log=${failed}/testBuildFailure.log
  # Not one 8-character piece of any secret, the base64 one too. The log
  # isn't shown when this fails: it would spread what it leaked.
  for s in ${lib.escapeShellArgs secrets} "$(${encode})"; do
    for ((i = 0; i + 8 <= ''${#s}; i++)); do
      if grep -qF -- "''${s:i:8}" "$log"; then
        echo "the log shows part of a secret"
        exit 1
      fi
    done
  done
  expect() {
    if [ "$(grep -cF -- "$1" "$log")" != "$2" ]; then
      echo "expected $2 findings of '$1' in the log:"; cat "$log"; exit 1
    fi
  }
  expect 'config.toml:1: github-pat ' 2
  expect 'deploy-key.pem:1: private-key ' 1
  expect 'encoded.txt:1: github-pat ' 1
  ${lib.concatMapStrings (f: "expect '${f}:1: github-pat ' 1\n") skipped}
  expect 'allowed.txt' 0
  expect 'ignored.txt' 0
  touch $out
''
