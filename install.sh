#!/usr/bin/env bash
# Same as deploy.sh; both names work.
exec "$(dirname "$(readlink -f "$0")")/deploy.sh" "$@"
