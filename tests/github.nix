# An App's tokens on a machine: its users get them through the socket, one
# who becomes a user at once, even in what ran from before, and nobody else
# does; nobody but the service reads the key or the tokens it
# keeps; and in the App's env, git and gh get the App's token, never the
# user's own login (its file, its keyring, its SSH keys, a header or URL of
# its in git's config), even when the App's can't be had. GitHub is a
# stand-in.
#
#   nix build .#checks.x86_64-linux.github -L
{ self, pkgs }:
let
  # GitHub's App API, as far as gh-token asks it.
  github = import ./github-app.nix { inherit pkgs; };
  # github.com's git, over https with a certificate of its own, and over
  # plain http: each request's logins are noted in /tmp/logins (Basic's,
  # decoded, a Kerberos ticket as `negotiate`, its cookies, and the client
  # certificate it was shown). Asked without one, it asks for one, as GitHub
  # does, and offers Negotiate too for a repo named kerberos. Over FTP, and
  # FTPS with that certificate, it notes each login it's offered, as
  # `ftp:USER:PASS`, and takes none.
  cert = pkgs.runCommand "github-cert" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey rsa:2048 -nodes -days 36500 -subj /CN=github.com \
      -addext subjectAltName=DNS:github.com -keyout $out/key.pem -out $out/cert.pem
    openssl req -x509 -newkey rsa:2048 -nodes -days 36500 -subj /CN=peter \
      -keyout $out/peter.key -out $out/peter.pem
  '';
  gitServer = pkgs.writers.writePython3 "fake-github-git" { flakeIgnore = [ "E501" ]; } ''
    import base64
    import socketserver
    import ssl
    import threading
    from http.server import BaseHTTPRequestHandler, HTTPServer


    class Git(BaseHTTPRequestHandler):
        def do_GET(self):
            auth = self.headers.get_all("Authorization") or []
            logins = ["negotiate" if a.startswith("Negotiate ") else base64.b64decode(a.split(" ", 1)[1]).decode() for a in auth]
            logins += ["cookie:" + c for c in self.headers.get_all("Cookie") or []]
            shown = getattr(self.connection, "getpeercert", lambda: None)()
            if shown:
                logins.append("cert:" + dict(x[0] for x in shown["subject"])["commonName"])
            with open("/tmp/logins", "a") as f:
                f.write(" ".join([self.path] + logins) + "\n")
            self.send_response(404 if auth else 401)
            if not auth:
                if "/kerberos" in self.path:
                    self.send_header("WWW-Authenticate", "Negotiate")
                self.send_header("WWW-Authenticate", 'Basic realm="GitHub"')
            self.send_header("Content-Length", "0")
            self.end_headers()


    class Ftp(socketserver.StreamRequestHandler):
        def handle(self):
            self.wfile.write(b"220 github.com\r\n")
            user = ""
            for line in self.rfile:
                command, _, arg = line.decode(errors="replace").strip().partition(" ")
                if command.upper() == "USER":
                    user = arg
                    self.wfile.write(b"331 password\r\n")
                elif command.upper() == "PASS":
                    with open("/tmp/logins", "a") as f:
                        f.write(f"ftp ftp:{user}:{arg}\n")
                    self.wfile.write(b"530 no\r\n")
                else:
                    self.wfile.write(b"530 log in first\r\n")


    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain("${cert}/cert.pem", "${cert}/key.pem")
    tls.verify_mode = ssl.CERT_OPTIONAL
    tls.load_verify_locations("${cert}/peter.pem")
    threading.Thread(target=HTTPServer(("127.0.0.1", 80), Git).serve_forever, daemon=True).start()
    threading.Thread(target=socketserver.ThreadingTCPServer(("127.0.0.1", 21), Ftp).serve_forever, daemon=True).start()
    ftps = socketserver.ThreadingTCPServer(("127.0.0.1", 990), Ftp)
    ftps.socket = tls.wrap_socket(ftps.socket, server_side=True)
    threading.Thread(target=ftps.serve_forever, daemon=True).start()
    server = HTTPServer(("127.0.0.1", 443), Git)
    server.socket = tls.wrap_socket(server.socket, server_side=True)
    server.serve_forever()
  '';
  # What gh would be run with: the token it was given, if any. Its help is
  # gh's, which says what the call's flags are.
  show = pkgs.writeShellScript "show" ''
    [ "$1" = help ] && exec ${pkgs.gh}/bin/gh "$@"
    echo "token=''${GH_TOKEN-} args=$*"
  '';
  # dev's login in the keyring, where `gh auth login` keeps it when there's
  # one: unkeyed, so gh finds it even without a config naming dev. Then gh
  # without a config, and gh in the App's env, in that session.
  keyring = env: pkgs.writeShellScript "keyring" ''
    set -u
    echo -n pw | ${pkgs.gnome-keyring}/bin/gnome-keyring-daemon --daemonize --unlock --components=secrets >/dev/null
    for _ in $(seq 50); do ${pkgs.libsecret}/bin/secret-tool search x y >/dev/null 2>&1 && break; sleep 0.2; done
    echo -n keyring-token | ${pkgs.libsecret}/bin/secret-tool store --label=gh service gh:github.com username ""
    echo "no config: $(GH_CONFIG_DIR=/run/gh-token/no-login ${pkgs.gh}/bin/gh auth token 2>&1)"
    echo "the App's env: $(env ${env} ${pkgs.gh}/bin/gh auth token 2>&1)"
  '';
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
    environment.systemPackages = [ config.burrow.tools.gh config.burrow.tools.gh-token config.burrow.tools.git-lfs config.programs.git.package pkgs.acl pkgs.krb5 pkgs.openssl ];
    users.users.dev = { isNormalUser = true; };
    users.users.eve = { isNormalUser = true; };
    # carol becomes one of its users with a switch, while her user manager,
    # and so every unit under it, runs from before.
    users.users.carol = { isNormalUser = true; uid = 1002; linger = true; };
    specialisation.carol.configuration.burrow.github.apps.workers.users = [ "dev" "carol" ];
    # The system from before ACLs, as a switch back to it finds it: a group,
    # and no unit that knows of ACLs.
    specialisation.groups.configuration = {
      users.groups.gh-token-workers.members = [ "dev" "eve" ];
      systemd.services.gh-token-workers-access.enable = false;
      systemd.sockets.gh-token-workers = {
        partOf = pkgs.lib.mkForce [ ];
        socketConfig = {
          ExecStartPost = pkgs.lib.mkForce [ ];
          SocketGroup = "gh-token-workers";
          SocketMode = pkgs.lib.mkForce "0660";
        };
      };
    };
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
      	helper = "!f() { echo username=peter; echo password=peter-token; }; f"
    '';
    systemd.services.fake-github = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = github;
    };
    # github.com, and github.com. as curl looks it up.
    networking.hosts."127.0.0.1" = [ "github.com" "github.com." ];
    security.pki.certificateFiles = [ "${cert}/cert.pem" ];
    systemd.services.fake-github-git = {
      wantedBy = [ "multi-user.target" ];
      serviceConfig.ExecStart = gitServer;
    };
    # A Kerberos realm github.com is in, so that dev can hold a ticket for it.
    environment.etc."krb5.conf".text = ''
      [libdefaults]
        default_realm = TEST
        dns_canonicalize_hostname = false
        rdns = false
      [realms]
        TEST = {
          kdc = 127.0.0.1
        }
      [domain_realm]
        github.com = TEST
    '';
    systemd.services.kdc = {
      wantedBy = [ "multi-user.target" ];
      environment.KRB5_KDC_PROFILE = pkgs.writeText "kdc.conf" ''
        [realms]
          TEST = {
            database_name = /var/lib/kdc/principal
            key_stash_file = /var/lib/kdc/stash
            acl_file = /var/lib/kdc/kadm5.acl
          }
      '';
      serviceConfig.StateDirectory = "kdc";
      script = ''
        touch /var/lib/kdc/kadm5.acl
        ${pkgs.krb5}/bin/kdb5_util create -s -r TEST -P master
        ${pkgs.krb5}/bin/kadmin.local -q "addprinc -pw peter-kerberos peter"
        ${pkgs.krb5}/bin/kadmin.local -q "addprinc -randkey HTTP/github.com"
        exec ${pkgs.krb5}/bin/krb5kdc -n
      '';
    };
  };

  testScript = { nodes, ... }:
    let
      env = builtins.concatStringsSep " " (pkgs.lib.mapAttrsToList (k: v: "${k}=${pkgs.lib.escapeShellArg v}")
        nodes.machine.burrow.github.apps.workers.env);
    in
    ''
      import base64
      import shlex

      ENV = ${builtins.toJSON env}

      # The whole command in the App's env, pipes and all.
      def dev(command, app=True, at="~"):
          return "su - dev -c " + shlex.quote(f"cd {at} && " + ("env " + ENV + " sh -c " + shlex.quote(command) if app else command))

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
          status, out = machine.execute(dev("gh-token get eve 2>&1"))
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
          # sam/r's own token, from before: not one for all of sam's repos.
          out = machine.succeed(dev(fill % "sam/r.git"))
          assert "username=x-access-token" in out and "password=ghs_2" in out and "peter" not in out, out
          # No token to be had: git stops, rather than take dev's login.
          status, out = machine.execute(dev(fill % "eve/x.git"))
          assert status != 0 and "peter" not in out, (status, out)

      with subtest("git, in the App's env, takes github.com however it's spelled, and stops for every other host"):
          # dev's own login for any host, and an askpass of dev's: neither is
          # asked, for any host.
          machine.succeed(dev("git config --global --add credential.helper '!f() { echo username=peter; echo password=peter-any; }; f' && printf '#!/bin/sh\\necho peter-askpass\\n' > ~/askpass && chmod +x ~/askpass", app=False))
          # The URL as printf's argument, never its format: `%67ithub.com` is one.
          def url_fill(url):
              return f"printf 'url=%s\\n\\n' {shlex.quote(url)} | GIT_ASKPASS=$HOME/askpass git credential fill"
          for url in [
              "https://GitHub.com/sam/r.git",
              "https://github.com:443/sam/r.git",
              "https://github.com:0443/sam/r.git",
              "https://github.com:/sam/r.git",
              "https://github.com./sam/r.git",
              "https://%67ithub.com/sam/r.git",
          ]:
              out = machine.succeed(dev(url_fill(url)))
              assert "password=ghs_2" in out and "peter" not in out, (url, out)
          for url in ["https://gist.github.com/abc.git", "https://WWW.GitHub.com/sam/r.git", "https://gitlab.com/sam/r.git", "http://github.com/sam/r.git"]:
              status, out = machine.execute(dev(url_fill(url) + " 2>&1"))
              assert status != 0 and "peter" not in out, (url, status, out)
              assert "peter-any" in machine.succeed(dev(url_fill(url), app=False)), url
          machine.succeed(dev("git config --global --unset-all credential.helper", app=False))

      with subtest("git, in the App's env, takes GitHub's SSH remotes over https, and never runs SSH"):
          for remote in ["git@github.com:sam/r.git", "ssh://git@github.com/sam/r.git"]:
              assert machine.succeed(dev(f"git ls-remote --get-url {remote}")).strip() == "https://github.com/sam/r.git"
              assert machine.succeed(dev(f"git ls-remote --get-url {remote}", app=False)).strip() == remote
          # Whatever would still take dev's SSH keys stops: an alias, a rewrite of dev's own.
          for call in ["git ls-remote gh:sam/r.git", "git -c url.git@github.com:.insteadOf=https://github.com/ ls-remote https://github.com/sam/r.git"]:
              status, out = machine.execute(dev(call + " 2>&1"))
              assert status != 0 and "never over SSH" in out, (status, out)
              status, out = machine.execute(dev(call + " 2>&1", app=False))
              assert "never over SSH" not in out, out

      with subtest("git, in the App's env, logs in to GitHub as the App only: no header or URL of dev's"):
          machine.wait_for_open_port(443)
          # dev's own logins, as a checkout's config can carry them: a header
          # for its repos, and a password in a remote's URL.
          header = "Authorization: Basic " + base64.b64encode(b"peter:header").decode()
          machine.succeed(dev(" && ".join([
              "git init -q own",
              f"git -C own config http.https://github.com/sam/r.git.extraHeader '{header}'",
              f"git -C own config http.https://github.com/eve/x.git.extraHeader '{header}'",
              "git -C own remote add origin https://github.com/sam/r.git",
              "git -C own remote add login https://peter:url-token@github.com/sam/q.git",
              "git -C own remote add ssh git@github.com:sam/r.git",
          ]), app=False))

          # The logins GitHub heard while git ran CALL.
          def logins(call, app=True):
              machine.succeed(": > /tmp/logins")
              machine.execute(dev(call, app=app, at="~/own"))
              return [login for line in machine.succeed("cat /tmp/logins").splitlines() for login in line.split(" ")[1:]]

          # Without the env, git sends them, the helper never asked.
          assert logins("git ls-remote origin", app=False) == ["peter:header"]
          assert logins("git ls-remote login", app=False) == ["peter:url-token"]
          # With it, only the App's: from git's config, a rewritten SSH
          # remote's, or the call's own -c.
          for call in [
              "git ls-remote origin",
              "git ls-remote login",
              "git ls-remote ssh",
              f"git -c 'http.https://github.com/sam/r.git.extraHeader={header}' ls-remote origin",
              f"git -c 'http.extraHeader={header}' ls-remote origin",
          ]:
              got = logins(call)
              assert got and all(login.startswith("x-access-token:ghs_") for login in got), (call, got)
          # No token to be had, or nowhere to ask: none at all.
          assert logins("git ls-remote https://github.com/eve/x.git") == []
          assert logins("GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote origin") == []

          # A header of dev's for every URL, and github.com as git and curl
          # also read it: with a trailing dot, escaped, over plain http.
          machine.succeed(dev(f"git config --global http.extraHeader '{header}'", app=False))
          spellings = ["https://github.com./sam/r.git", "https://%67ithub.com/sam/r.git", "http://github.com/sam/r.git", "http://github.com./sam/r.git"]
          for url in spellings:
              assert set(logins(f"git ls-remote {url}", app=False)) == {"peter:header"}, url
          # With the env, the App's over https, and none over plain http.
          for url in spellings[:2]:
              got = logins(f"git ls-remote {url}")
              assert got and all(login.startswith("x-access-token:ghs_") for login in got), (url, got)
          for url in spellings[2:]:
              assert logins(f"git ls-remote {url}") == [], url
          assert logins(f"GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote {spellings[1]}") == []
          machine.succeed(dev("git config --global --unset http.extraHeader", app=False))

      with subtest("git, in the App's env, logs in to GitHub as the App only: no netrc of dev's"):
          # dev's netrc, for github.com, and another that NETRC names.
          machine.succeed(dev(" && ".join([
              "printf 'machine github.com login peter password netrc\\n' > ~/.netrc",
              "printf 'default login peter password netrc-default\\n' > ~/other.netrc",
              "chmod 600 ~/.netrc ~/other.netrc",
              "git -C own remote add plain https://github.com/sam/p.git",
              "git -C own remote add plain-ssh git@github.com:sam/p.git",
              "git -C own -c user.name=dev -c user.email=dev@example.org commit -q --allow-empty -m x",
          ]), app=False))
          # Without the env, curl answers GitHub's 401 with it, the helper never asked.
          assert logins("git ls-remote plain", app=False) == ["peter:netrc"]
          assert logins("NETRC=$HOME/other.netrc git ls-remote plain", app=False) == ["peter:netrc-default"]
          # With it, only the App's: fetch and push, over https or SSH rewritten.
          for call in [
              "git ls-remote plain",
              "git ls-remote plain-ssh",
              "git push plain HEAD:refs/heads/x",
              "NETRC=$HOME/other.netrc git ls-remote plain",
              "git ls-remote https://%67ithub.com/sam/p.git",
          ]:
              got = logins(call)
              assert got and all(login.startswith("x-access-token:ghs_") for login in got), (call, got)
          assert logins("GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote plain") == []
          assert logins("GH_TOKEN_SOCKET=/run/gh-token/none.sock git push plain HEAD:refs/heads/x") == []
          # FTP and FTPS, the other names of git's curl helper, from the
          # URL, a rewrite, or a remote's own helper: dev's logins without
          # the env, an anonymous one with it, with a broker or none.
          machine.succeed(dev(" && ".join([
              "git -C own config url.ftp://github.com/.insteadOf https://ftp.example/",
              "git -C own remote add viaftps https://github.com/sam/p.git",
              "git -C own config remote.viaftps.vcs ftps",
          ]), app=False))
          assert logins("git ls-remote ftp://github.com/sam/p.git", app=False) == ["ftp:peter:netrc"]
          assert logins("git ls-remote ftps://github.com/sam/p.git", app=False) == ["ftp:peter:netrc"]
          assert logins("git ls-remote ftp://peter:url-token@github.com/sam/p.git", app=False) == ["ftp:peter:url-token"]
          for call in [
              "git ls-remote ftp://github.com/sam/p.git",
              "git ls-remote ftps://github.com/sam/p.git",
              "git ls-remote ftp://peter:url-token@github.com/sam/p.git",
              "git ls-remote https://ftp.example/sam/p.git",
              "git ls-remote viaftps",
              "GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote ftp://peter:url-token@github.com/sam/p.git",
          ]:
              got = logins(call)
              assert got and not any("peter" in login for login in got), (call, got)

      with subtest("git, in the App's env, logs in to GitHub as the App only: no cookie, certificate or ticket of dev's"):
          # What git has curl log in with from the first request, asking no
          # helper: dev's cookies, dev's client certificate, and the empty
          # login that answers Negotiate with dev's Kerberos ticket.
          machine.wait_for_open_port(88)
          peter = "${cert}/peter"
          machine.succeed(dev(" && ".join([
              "echo peter-kerberos | kinit peter",
              "printf '.github.com\\tTRUE\\t/\\tFALSE\\t0\\tuser_session\\tpeter\\n' > ~/cookies",
              "git config --global http.cookieFile ~/cookies",
              f"git config --global http.sslCert {peter}.pem",
              f"git config --global http.sslKey {peter}.key",
              "git config --global http.emptyAuth true",
          ]), app=False))
          kerberos = "https://github.com/sam/kerberos.git"
          # Without the env, curl sends them.
          assert {"cookie:user_session=peter", "cert:peter"} <= set(logins("git ls-remote origin", app=False))
          assert {"negotiate", "cookie:user_session=peter", "cert:peter"} <= set(logins(f"git ls-remote {kerberos}", app=False))
          # With it, only the App's: from git's config, the call's -c for
          # exactly its URL, a URL git can't read, or git's own variables.
          exact = " ".join(f"-c http.https://github.com/sam/r.git.{k}" for k in [f"sslCert={peter}.pem", f"sslKey={peter}.key", "cookieFile=$HOME/cookies", "emptyAuth=true"])
          for call in [
              "git ls-remote origin",
              f"git ls-remote {kerberos}",
              f"git {exact} ls-remote origin",
              "git ls-remote https://%67ithub.com/sam/r.git",
              f"GIT_SSL_CERT={peter}.pem GIT_SSL_KEY={peter}.key git ls-remote origin",
          ]:
              got = logins(call)
              assert got and all(login.startswith("x-access-token:ghs_") for login in got), (call, got)
          # Nowhere to ask for a token, or plain http: none at all, from
          # the first request.
          for call in [
              "GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote origin",
              f"GH_TOKEN_SOCKET=/run/gh-token/none.sock git ls-remote {kerberos}",
              "git ls-remote http://github.com/sam/r.git",
          ]:
              assert logins(call) == [], call
          # A bundle's download, which git runs the helper for with the URL
          # alone: the App's token, and none of dev's; and not at all for a
          # URL with a login, which git sends again where it can't be dropped.
          def heard(call, app=True):
              machine.succeed(": > /tmp/logins")
              machine.execute(dev(call, app=app, at="~/own"))
              return machine.succeed("cat /tmp/logins").splitlines()
          got = heard("git -c fetch.bundleURI=https://github.com/sam/r.bundle fetch origin")
          assert any(line.startswith("/sam/r.bundle x-access-token:ghs_") for line in got), got
          assert not any("peter" in line for line in got), got
          got = heard("git -c fetch.bundleURI=https://peter:url-token@github.com/sam/r.bundle fetch origin")
          assert not any(line.startswith("/sam/r.bundle") or "peter" in line for line in got), got
          # git's helper reads its settings for the URL its config resolves
          # the bundle's to, while the bundle is asked for as given: a URL
          # rewritten to another host, a remote named like it, and a rewrite
          # that adds a login. Without the env, dev's header and login for
          # those URLs go to GitHub; with it, none.
          own = "git -C own config"
          resolved = [
              f"{own} url.https://eve.example/b/.insteadOf https://github.com/sam/rewritten.bundle",
              f"{own} http.https://eve.example/.extraHeader '{header}'",
              f"{own} remote.https://github.com/sam/named.bundle.url https://eve.example/c",
              f"{own} url.https://peter:rewritten@login.example/.insteadOf https://github.com/sam/login.bundle",
          ]
          machine.succeed(dev(" && ".join(resolved), app=False))
          for bundle, theirs in [("rewritten", "peter:header"), ("named", "peter:header"), ("login", "peter:rewritten")]:
              call = f"git -c fetch.bundleURI=https://github.com/sam/{bundle}.bundle fetch origin"
              got = heard(call, app=False)
              assert any(line.startswith(f"/sam/{bundle}.bundle") and theirs in line for line in got), (bundle, got)
              got = heard(call)
              assert any(line.startswith(f"/sam/{bundle}.bundle x-access-token:ghs_") for line in got), (bundle, got)
              assert not any("peter" in line for line in got), (bundle, got)
              got = heard("GH_TOKEN_SOCKET=/run/gh-token/none.sock " + call)
              assert not any("peter" in line for line in got), (bundle, got)
          machine.succeed(dev(" && ".join(f"{own} --remove-section {s}" for s in [
              "url.https://eve.example/b/", "http.https://eve.example/", "remote.https://github.com/sam/named.bundle",
              "url.https://peter:rewritten@login.example/",
          ]), app=False))
          machine.succeed(dev("git config --global --remove-section http && kdestroy", app=False))

      with subtest("git-lfs isn't run in the App's env, however git or a config starts it"):
          assert "git-lfs/" in machine.succeed(dev("git lfs version", app=False))
          machine.succeed(dev(" && ".join([
              "printf '*.bin filter=lfs -text\\n' > .gitattributes",
              "echo data > x.bin",
              "printf '#!/bin/sh\\ngit lfs pre-push \"$@\"\\n' > .git/hooks/pre-push",
              "printf '#!/bin/sh\\ngit-lfs pre-push \"$@\"\\n' > .git/hooks/post-checkout",
              "chmod +x .git/hooks/pre-push .git/hooks/post-checkout",
          ]), app=False, at="~/own"))
          for call in [
              "git lfs version",
              "git-lfs version",
              "git hook run pre-push -- origin https://github.com/sam/r.git </dev/null",
              "git hook run post-checkout -- HEAD HEAD 1",
              # A filter: the file goes in neither as it is nor as LFS's.
              "git add x.bin",
              "git -c filter.lfs.required=false add x.bin",
          ]:
              status, out = machine.execute(dev(call + " 2>&1", at="~/own"))
              assert status != 0 and "git-lfs isn't run" in out, (call, status, out)
              assert machine.succeed(dev("git ls-files x.bin", at="~/own")).strip() == "", call
          # A checkout's config naming another git-lfs, the one LFS installs, say.
          machine.succeed(dev("git -C own config filter.lfs.process '${pkgs.git-lfs}/bin/git-lfs filter-process'", app=False))
          status, out = machine.execute(dev("git add x.bin 2>&1", at="~/own"))
          assert status != 0 and "git-lfs isn't run" in out, (status, out)
          assert machine.succeed(dev("git ls-files x.bin", at="~/own")).strip() == ""

      with subtest("gh, in the App's env, has the App's token, and no login of dev's"):
          assert machine.succeed(dev("gh auth token", app=False)).strip() == "peter-token"
          machine.fail(dev("${pkgs.gh}/bin/gh auth token"))
          machine.fail("su - dev -c " + shlex.quote("GITHUB_TOKEN=peter-token env " + ENV + " ${pkgs.gh}/bin/gh auth token"))
          # Nor another host's, in the variables gh reads for one.
          for var in ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"]:
              ghe = var + "=peter-token ${pkgs.gh}/bin/gh auth token --hostname ghe.example.com"
              assert machine.succeed(dev(ghe, app=False)).strip() == "peter-token"
              machine.fail("su - dev -c " + shlex.quote(var + "=peter-token env " + ENV + " " + ghe.split(" ", 1)[1]))
          # In dev's keyring: gh finds it with no config, but not in the App's env.
          out = machine.succeed(dev("${pkgs.dbus}/bin/dbus-run-session -- ${keyring env}", app=False))
          assert "no config: keyring-token" in out and "the App's env: no oauth token" in out, out
          out = machine.succeed(dev("gh-token gh ${show} pr view 5 -R sam/r")).strip()
          assert out == "token=ghs_1 args=pr view 5 -R sam/r", out
          # The call read as gh reads it, with gh's help: the command, aliases
          # and flags before it too, which flags take a value (that value is
          # data), and what each argument is.
          for call in ["issue -R sam/r comment 5 --body https://github.com/eve/x", "repo ls --limit 5 sam", "api -- repos/sam/r", "pr view 5 -R=sam/r", "search issues https://github.com/eve/x --owner sam"]:
              assert machine.succeed(dev(f"gh-token gh ${show} {call}")).strip() == f"token=ghs_1 args={call}"
          # gh's help read plain, whatever colours the call asks for.
          out = machine.succeed(dev("CLICOLOR_FORCE=1 GH_FORCE_TTY=1 gh-token gh ${show} pr view 5 -R sam/r")).strip()
          assert out == "token=ghs_1 args=pr view 5 -R sam/r", out
          # Another host isn't the App's, however it's named.
          for call in ["GH_HOST=ghe.example.com gh-token gh ${show} pr view 5 -R sam/r", "gh-token gh ${show} api --hostname ghe.example.com repos/sam/r", "gh-token gh ${show} pr view https://ghe.example.com/sam/r/pull/5"]:
              status, out = machine.execute(dev(call + " 2>&1"))
              assert status == 1 and "the App's token is for github.com" in out, (status, out)
          # A token given already is the one it runs with; an empty one isn't one.
          assert machine.succeed(dev("GH_TOKEN=mine gh-token gh ${show} api user")).strip() == "token=mine args=api user"
          out = machine.succeed(dev("GH_TOKEN= GITHUB_TOKEN=peter-token gh-token gh ${show} api repos/sam/r")).strip()
          assert out == "token=ghs_1 args=api repos/sam/r", out
          # One the shell had before it entered the App's env is the user's, and goes.
          out = machine.succeed("su - dev -c " + shlex.quote("GH_TOKEN=peter-token env " + ENV + " gh-token gh ${show} api repos/sam/r")).strip()
          assert out == "token=ghs_1 args=api repos/sam/r", out
          # The repo gh acts on: a fork's upstream, unless the call names another.
          machine.succeed(dev("git init -q fork && git -C fork remote add origin git@github.com:eve/r.git && git -C fork remote add upstream https://github.com/sam/r.git", app=False))
          assert machine.succeed(dev("gh-token gh ${show} pr view 5", at="~/fork")).strip() == "token=ghs_1 args=pr view 5"
          status, out = machine.execute(dev("gh-token gh ${show} pr view https://github.com/eve/r/pull/5 2>&1", at="~/fork"))
          assert status == 1 and "isn't installed on eve" in out, (status, out)
          # Outside the App's env, gh is just gh.
          assert machine.succeed(dev("gh-token gh ${show} api user", app=False)).strip() == "token= args=api user"
          # The wrapper on the PATH is this.
          assert "peter" not in machine.succeed(dev("gh auth status 2>&1 || true"))

      gh_token = "GH_TOKEN_SOCKET=/run/gh-token/workers.sock ${nodes.machine.burrow.tools.gh-token}/bin/gh-token get sam 2>&1"
      # Asked from carol's user manager, which started before any switch.
      carol = f"systemd-run --machine=carol@.host --user --wait --pipe --quiet -E {gh_token}"
      # Asked from a fresh login, with the groups the system says now.
      eve = "su - eve -c " + shlex.quote(gh_token)
      socket = "/run/gh-token/workers.sock"

      def switch(to):
          path = f"/run/booted-system/specialisation/{to}" if to else "/run/booted-system"
          machine.succeed(f"{path}/bin/switch-to-configuration test")

      def allowed(ask):
          out = machine.succeed(ask)
          assert out.strip().startswith("ghs_"), out

      def denied(ask):
          status, out = machine.execute(ask)
          assert status != 0 and "Permission denied" in out, (status, out)

      def owned(expected):
          assert machine.succeed(f"stat -c '%U:%G %a' {socket}").strip() == expected
          return machine.succeed(f"getfacl -c {socket}")

      with subtest("one who becomes a user gets tokens at once, in what was running before, and stops with the switch that ends it"):
          machine.wait_for_unit("user@1002.service")
          denied(carol)
          inode = machine.succeed(f"stat -c %i {socket}")
          switch("carol")
          allowed(carol)
          switch(None)
          denied(carol)
          # A change of users never takes the socket down.
          assert machine.succeed(f"stat -c %i {socket}") == inode
          # A socket made again, as at boot, is its users' again.
          machine.succeed("systemctl restart gh-token-workers.socket")
          allowed(dev("gh-token get sam"))
          status, out = machine.execute(carol)
          assert status != 0 and "ghs_" not in out, (status, out)

      with subtest("a socket is ready only once its users can connect, whatever else has run"):
          machine.succeed("systemctl stop gh-token-workers-access.service")
          machine.succeed("systemctl start gh-token-workers.socket")
          machine.fail("systemctl is-active gh-token-workers-access.service")
          assert "user:dev:rw-" in owned("root:root 660")
          allowed(dev("gh-token get sam"))
          machine.succeed("systemctl start gh-token-workers-access.service")

      with subtest("a switch back to a system from before ACLs leaves its socket as that system says"):
          switch("carol")
          allowed(carol)
          switch("groups")
          acl = owned("root:gh-token-workers 660")
          assert "user:" not in acl.replace("user::", ""), acl
          denied(carol)
          allowed(eve)
          allowed(dev("gh-token get sam"))

      with subtest("the first switch from a system before ACLs sets them, with a change of users in it"):
          switch("carol")
          acl = owned("root:root 660")
          assert "user:carol:rw-" in acl and "group::---" in acl, acl
          allowed(carol)
          allowed(dev("gh-token get sam"))
          denied(eve)
          # And the same from the system's own users, without carol.
          switch("groups")
          switch(None)
          assert "user:carol" not in owned("root:root 660")
          denied(carol)
          denied(eve)
          allowed(dev("gh-token get sam"))

      with subtest("a switch brings back an access unit that isn't running, with a change of users in it"):
          machine.succeed("systemctl stop gh-token-workers-access.service")
          machine.succeed("systemctl start gh-token-workers.socket")
          machine.fail("systemctl is-active gh-token-workers-access.service")
          switch("carol")
          machine.succeed("systemctl is-active gh-token-workers-access.service")
          allowed(carol)
          switch(None)
          denied(carol)
    '';
}
