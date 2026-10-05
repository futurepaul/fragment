#!/bin/sh
# simplex-chat's bot API (-p) listens on 127.0.0.1 only; the spike's host
# side reaches it through this forward on 0.0.0.0:$API_PUBLIC_PORT.
set -eu
if [ -n "${API_PUBLIC_PORT:-}" ]; then
  socat "TCP-LISTEN:${API_PUBLIC_PORT},fork,reuseaddr" "TCP:127.0.0.1:${API_PORT:-5225}" &
fi
exec /usr/local/bin/simplex-chat "$@"
