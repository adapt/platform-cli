#!/usr/bin/env python3
"""Daemon fixture: exercise process bootstrap without requiring a hypervisor."""
import argparse
import json
import os
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

parser = argparse.ArgumentParser()
for flag in ("state-dir", "listen", "worker", "token-file", "ready-file"):
    parser.add_argument("--" + flag, required=True)
args = parser.parse_args()
assert "ADAPT_TOKEN" not in os.environ
state = Path(args.state_dir)
token = Path(args.token_file).read_text()
instance = "22222222-2222-4222-8222-222222222222"
with (state / "starts").open("a") as stream:
    stream.write("started\n")


class Handler(BaseHTTPRequestHandler):
    def respond(self, status, value):
        assert self.headers["Authorization"] == "Bearer " + token
        assert "X-Organization-Id" not in self.headers
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.respond(200, {
            "version": 1, "instance_id": instance, "runtime_version": "fixture",
            "features": {"sessions": True, "image_pull": True},
        })

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        self.respond(400, {"error": "fixture rejected image"})

    def do_DELETE(self):
        self.respond(404, {"error": "no VM was created"})

    def log_message(self, *_):
        pass


host, port = args.listen.rsplit(":", 1)
server = HTTPServer((host, int(port)), Handler)
ready = Path(args.ready_file)
temporary = ready.with_suffix(".tmp")
temporary.write_text(json.dumps({
    "version": 1, "address": "%s:%s" % server.server_address,
    "pid": os.getpid(), "instance_id": instance, "runtime_version": "fixture",
}))
temporary.chmod(0o600)
temporary.replace(ready)
server.serve_forever()
