#!/bin/sh
set -eu

APP_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$APP_DIR"

if [ ! -x ./movex ]; then
  chmod 0755 ./movex
fi

# Most PaaS platforms expose the assigned HTTP port as PORT. MOVEX currently
# reads PORT and defaults to 3000 when the variable is absent.
: "${PORT:=3000}"
export PORT

exec ./movex serve
