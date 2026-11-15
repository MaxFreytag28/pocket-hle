#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
"""Development web server for the web build of touchHLE.

Serves a folder (as made by web-dist.ps1) on localhost only, with caching
turned off so a rebuilt .wasm is always picked up.

It also accepts screenshots from the page: with ?debug in the address,
web/index.html defines pocketHleShot(name), which posts the current frame
here, and it is written to <shots folder>/<name>.png. This is how the web
build can be looked at from scripts.

Usage: web-serve.py [port] [folder] [shots folder]
"""

import functools
import http.server
import os
import re
import sys
import urllib.parse


class Handler(http.server.SimpleHTTPRequestHandler):
    shots_dir = "web-shots"

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        url = urllib.parse.urlparse(self.path)
        if url.path != "/__shot":
            self.send_error(404)
            return
        name = urllib.parse.parse_qs(url.query).get("name", ["shot"])[0]
        # The name becomes a file name, so keep it harmless.
        name = re.sub(r"[^A-Za-z0-9_-]", "", name)[:64] or "shot"
        length = int(self.headers.get("Content-Length", "0"))
        if not 0 < length <= 16 * 1024 * 1024:
            self.send_error(400)
            return
        data = self.rfile.read(length)
        os.makedirs(self.shots_dir, exist_ok=True)
        with open(os.path.join(self.shots_dir, name + ".png"), "wb") as f:
            f.write(data)
        self.send_response(204)
        self.end_headers()

    def log_message(self, format, *args):
        # Only errors are interesting.
        if args and str(args[1] if len(args) > 1 else "").startswith(("4", "5")):
            super().log_message(format, *args)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    folder = sys.argv[2] if len(sys.argv) > 2 else "web-dist"
    Handler.shots_dir = sys.argv[3] if len(sys.argv) > 3 else "web-shots"
    handler = functools.partial(Handler, directory=folder)
    with http.server.ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"Serving {folder} at http://127.0.0.1:{port}/ (screenshots go to {Handler.shots_dir})")
        sys.stdout.flush()
        server.serve_forever()


if __name__ == "__main__":
    main()
