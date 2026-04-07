#!/bin/sh
set -eu

if [ -z "${CONSOLE_BASIC_AUTH_USERNAME:-}" ] || [ -z "${CONSOLE_BASIC_AUTH_PASSWORD:-}" ]; then
  echo "CONSOLE_BASIC_AUTH_USERNAME and CONSOLE_BASIC_AUTH_PASSWORD are required for public console deploy" >&2
  exit 1
fi

export EXPECTED_AUTH_HEADER="Basic $(printf "%s:%s" "$CONSOLE_BASIC_AUTH_USERNAME" "$CONSOLE_BASIC_AUTH_PASSWORD" | base64 | tr -d '\n')"

if [ -f /etc/nginx/conf.d/default.conf ]; then
  sed -i "s|\\\${EXPECTED_AUTH_HEADER}|$EXPECTED_AUTH_HEADER|g" /etc/nginx/conf.d/default.conf
fi
