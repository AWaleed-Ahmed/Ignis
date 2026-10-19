"""Owned loopback smart-HTTP Git fixture using synthetic helper credentials."""
from __future__ import annotations

import argparse
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import threading
from urllib.parse import urlsplit


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("status", type=Path)
    args = parser.parse_args()
    stats = {"unauthorized": 0, "authorized": 0, "uploads": 0}
    lock = threading.Lock()
    authorization = "Basic " + base64.b64encode(b"fixture-user:fixture-pass").decode()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_args) -> None:
            pass

        def do_GET(self) -> None:
            self.exchange()

        def do_POST(self) -> None:
            self.exchange()

        def exchange(self) -> None:
            authenticated = self.headers.get("Authorization") == authorization
            with lock:
                stats["authorized" if authenticated else "unauthorized"] += 1
                if authenticated and self.command == "POST":
                    stats["uploads"] += 1
                args.status.write_text(json.dumps({"port": self.server.server_port, **stats}))
            if not authenticated:
                self.send_response(401)
                self.send_header("WWW-Authenticate", 'Basic realm="owned-fixture"')
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            url = urlsplit(self.path)
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            env = dict(os.environ, GIT_PROJECT_ROOT=str(args.root), GIT_HTTP_EXPORT_ALL="1",
                       PATH_INFO=url.path, QUERY_STRING=url.query, REQUEST_METHOD=self.command,
                       CONTENT_TYPE=self.headers.get("Content-Type", ""), CONTENT_LENGTH=str(len(body)),
                       REMOTE_USER="fixture-user", HTTP_GIT_PROTOCOL=self.headers.get("Git-Protocol", ""))
            result = subprocess.run(["git", "http-backend"], input=body, capture_output=True,
                                    env=env, timeout=10, check=True)
            headers, payload = result.stdout.split(b"\r\n\r\n", 1)
            parsed = [line.decode().split(":", 1) for line in headers.split(b"\r\n")]
            status = next((int(value.strip().split()[0]) for key, value in parsed if key.lower() == "status"), 200)
            self.send_response(status)
            for key, value in parsed:
                if key.lower() != "status":
                    self.send_header(key, value.strip())
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.status.write_text(json.dumps({"port": server.server_port, **stats}))
    server.serve_forever()


if __name__ == "__main__":
    main()
