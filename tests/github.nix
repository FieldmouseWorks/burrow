# An App's tokens on a machine: its users get them through the socket, and
# nobody else does; nobody but the service reads the key or the tokens it
# keeps; and in the App's env, git and gh get the App's token, never the
# user's own login, even when the App's can't be had. GitHub is a stand-in.
#
#   nix build .#checks.x86_64-linux.github -L
{ self, pkgs }:
let
  # GitHub's App API, as far as gh-token asks it: sam has the App, eve
  # doesn't. Each token is new, and each request is noted in /tmp/asked.
  github = pkgs.writers.writePython3 "fake-github" { flakeIgnore = [ "E501" ]; } ''
    import datetime
    import json
    from http.server import BaseHTTPRequestHandler, HTTPServer

    minted = [0]


    class GitHub(BaseHTTPRequestHandler):
        def answer(self, status, body):
            with open("/tmp/asked", "a") as f:
                f.write(f"{self.command} {self.path}\n")
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if not self.headers.get("Authorization", "").startswith("Bearer "):
                return self.answer(401, {"message": "no JWT"})
            self.answer(200, [{"id": 42, "account": {"login": "sam"}}])

        def do_POST(self):
            self.rfile.read(int(self.headers["Content-Length"]))
            minted[0] += 1
            at = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=1)
            self.answer(201, {"token": f"ghs_{minted[0]}", "expires_at": at.strftime("%Y-%m-%dT%H:%M:%SZ")})


    HTTPServer(("127.0.0.1", 8080), GitHub).serve_forever()
  '';
  # What gh would be run with: the token it was given, if any.
  show = pkgs.writeShellScript "show" ''echo "token=''${GH_TOKEN-} args=$*"'';
in
pkgs.testers.runNixOSTest {
  name = "github";

  nodes.machine = { config, ... }: {
    imports = [ self.nixosModules.github ];
    burrow.github.apps.workers = {
      id = 7;
      slug = "sam-workers";
      keyFile = "/run/keys/workers.pem";
      api = "http://127.0.0.1:8080";
    };
    environment.systemPackages = [ config.burrow.tools.gh config.burrow.tools.gh-token pkgs.git pkgs.openssl ];
    users.users.dev = { isNormalUser = true; };
    users.users.eve = { isNormalUser = true; };
    # dev's own logins: gh's, and a git helper the way `gh auth setup-git`
    # leaves one, its empty helper clearing the system's.
    environment.etc."dev/hosts.yml".text = ''
      github.com:
          oauth_token: peter-token
          user: peter
    '';
    environment.etc."dev/gitconfig".text = ''
      [credential "https://github.com"]
      	helper =
      	helper = "!f() { echo password=peter-token; }; f"
    '';
    systemd.services.fake-github = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = github;
    };
  };

  testScript = { nodes, ... }:
    let
      env = builtins.concatStringsSep " " (pkgs.lib.mapAttrsToList (k: v: "${k}=${pkgs.lib.escapeShellArg v}")
        nodes.machine.burrow.github.apps.workers.env);
    in
    ''
      import shlex

      ENV = ${builtins.toJSON env}

      def dev(command, app=True):
          return "su - dev -c " + shlex.quote(("env " + ENV + " " if app else "") + command)

      def asked():
          return machine.succeed("cat /tmp/asked").splitlines()

      machine.wait_for_unit("multi-user.target")
      machine.succeed("install -d -m 0700 /run/keys && openssl genrsa -traditional -out /run/keys/workers.pem 2048 && chmod 0400 /run/keys/workers.pem")
      machine.wait_for_open_port(8080)

      with subtest("its users get a token, kept until it's nearly out"):
          assert machine.succeed(dev("gh-token get sam")).strip() == "ghs_1"
          assert asked() == ["GET /app/installations?per_page=100&page=1", "POST /app/installations/42/access_tokens"], asked()
          assert machine.succeed(dev("gh-token get Sam")).strip() == "ghs_1"
          assert len(asked()) == 2, asked()
          assert machine.succeed(dev("gh-token get sam/r")).strip() == "ghs_2"
          # An account without the App: refused, not "try later".
          status, out = machine.execute(dev("gh-token get eve"))
          assert status == 1 and "isn't installed on eve" in out, (status, out)

      with subtest("nobody else, and nobody reads the key or what's kept"):
          status, out = machine.execute("su - eve -c 'GH_TOKEN_SOCKET=/run/gh-token/workers.sock gh-token get sam'")
          assert status != 0 and "ghs_" not in out, (status, out)
          machine.fail(dev("cat /run/keys/workers.pem"))
          machine.fail(dev("ls /run/gh-token-cache/workers"))
          machine.fail(dev("cat /run/gh-token-cache/workers/sam"))

      machine.succeed(dev("mkdir -p ~/.config/gh && cp /etc/dev/hosts.yml ~/.config/gh/ && cp /etc/dev/gitconfig ~/.gitconfig", app=False))
      fill = "printf 'protocol=https\\nhost=github.com\\npath=%s\\n\\n' | git credential fill"

      with subtest("git, in the App's env, has the App's token for the repo it names"):
          assert "password=peter-token" in machine.succeed(dev(fill % "sam/r.git", app=False))
          out = machine.succeed(dev(fill % "sam/r.git"))
          assert "username=x-access-token" in out and "password=ghs_1" in out and "peter" not in out, out
          # No token to be had: git stops, rather than take dev's login.
          status, out = machine.execute(dev(fill % "eve/x.git"))
          assert status != 0 and "peter" not in out, (status, out)

      with subtest("gh, in the App's env, has the App's token, and no login of dev's"):
          assert machine.succeed(dev("gh auth token", app=False)).strip() == "peter-token"
          machine.fail(dev("${pkgs.gh}/bin/gh auth token"))
          out = machine.succeed(dev("gh-token gh ${show} pr view 5 -R sam/r")).strip()
          assert out == "token=ghs_1 args=pr view 5 -R sam/r", out
          # A token given already is the one it runs with.
          assert machine.succeed(dev("GH_TOKEN=mine gh-token gh ${show} api user")).strip() == "token=mine args=api user"
          # Outside the App's env, gh is just gh.
          assert machine.succeed(dev("gh-token gh ${show} api user", app=False)).strip() == "token= args=api user"
          # The wrapper on the PATH is this.
          assert "peter" not in machine.succeed(dev("gh auth status 2>&1 || true"))
    '';
}
