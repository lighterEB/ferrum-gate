#!/bin/sh
set -eu

if [ -z "${CONSOLE_BASIC_AUTH_USERNAME:-}" ] || [ -z "${CONSOLE_BASIC_AUTH_PASSWORD:-}" ]; then
  echo "CONSOLE_BASIC_AUTH_USERNAME and CONSOLE_BASIC_AUTH_PASSWORD are required for public console deploy" >&2
  exit 1
fi

htpasswd -bc /etc/nginx/.htpasswd "$CONSOLE_BASIC_AUTH_USERNAME" "$CONSOLE_BASIC_AUTH_PASSWORD"
