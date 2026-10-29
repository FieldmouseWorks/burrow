# GitHub's App API, as far as gh-token asks it, for the VM tests: sam has
# the App, eve doesn't. Each token is new, and each request is noted in
# /tmp/asked. It listens on 127.0.0.1:8080, the `api` the tests give an App.
{ pkgs }:
pkgs.writers.writePython3 "fake-github" { flakeIgnore = [ "E501" ]; } ''
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
''
