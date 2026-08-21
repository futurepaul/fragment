#!/usr/bin/env python3
"""Caddy on-demand-TLS ask gate: allow exactly fragment.club, www, and
one-label *.fragment.club names. Anything else (other people's domains
pointed at this box) is refused, so we can't be used to burn LE quota."""
import re
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse

BASE = "fragment.club"
LABEL = re.compile(r"^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$")


class Ask(BaseHTTPRequestHandler):
    def do_GET(self):
        domain = (parse_qs(urlparse(self.path).query).get("domain") or [""])[0].lower()
        ok = domain in (BASE, "www." + BASE) or (
            domain.endswith("." + BASE)
            and len(domain.split(".")) == 3
            and LABEL.match(domain.split(".")[0])
        )
        self.send_response(200 if ok else 404)
        self.end_headers()

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 9999), Ask).serve_forever()
