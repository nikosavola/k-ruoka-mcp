#!/bin/sh
# Makes a login's debug port reachable from outside the container.
#
# Chrome's remote-debugging HTTP server validates the actual TCP peer of a connection,
# not just its listen address or Host header -- so `--remote-debugging-address=0.0.0.0`
# alone does not help: a connection arriving through Docker's port-publish NAT still
# gets refused, because its peer address is not literally loopback. Verified directly:
# with Chrome bound to 0.0.0.0, a request from the host still failed with "connection
# reset by peer"; only a same-network-namespace relay resolved it.
#
# socat is the relay. It runs in this same container (same netns as Chrome), listens
# on the container's own address, and opens a *fresh* connection to Chrome's own
# 127.0.0.1 -- which Chrome accepts, because that connection genuinely originates from
# loopback. Chrome itself stays on its safe default; nothing here widens what Chrome
# will bind to.
#
# Binding the container's own address rather than 0.0.0.0: also verified directly,
# binding the wildcard address to the same port number Chrome already holds on
# 127.0.0.1 fails outright ("Address in use") on this network stack, wildcard and
# loopback binds are not as independent as they are on a bare host. The container's own
# address is a different, specific address, so it does not collide -- and it is what
# Docker's port-publish actually forwards to regardless.
#
# The watcher runs for `serve` too, not just the `login` subcommand, because the
# start_login MCP tool launches a login *later*, inside a running serve -- so there is
# no argument at container start that says a debug port will ever be needed. It idles
# until one appears, and re-arms if that Chrome goes away and a second login starts.
#
# Waiting for Chrome first is load-bearing, not just tidy: `login` refuses to start when
# its debug port is already taken (guarding against a leftover Chrome), and it checks
# before launching Chrome. Binding the relay early would trip that guard against itself.
set -eu

debug_port_from_args() {
    port=9222
    prev=""
    for arg in "$@"; do
        case "$prev" in
        --port) port="$arg" ;;
        esac
        case "$arg" in
        --port=*) port="${arg#--port=}" ;;
        esac
        prev="$arg"
    done
    printf '%s' "$port"
}

# An explicit `login --port` wins; otherwise K_RUOKA_DEBUG_PORT, else the same default
# the tool and the subcommand use. With `serve` there is no port in the arguments at all,
# because start_login picks one later -- so in a container, publish this port and let
# start_login use the default rather than passing one it cannot know about.
port=$(debug_port_from_args "$@")
if [ "$port" = 9222 ]; then
    port="${K_RUOKA_DEBUG_PORT:-9222}"
fi
container_ip=$(hostname -i 2>/dev/null | awk '{print $1}')

if [ -n "$container_ip" ]; then
    (
        while :; do
            if wget -q --spider "http://127.0.0.1:${port}/json/version" 2>/dev/null; then
                # Foreground, so this loop only comes back around once the relay exits,
                # i.e. once that Chrome is gone and a later login could need a new one.
                socat "TCP-LISTEN:${port},bind=${container_ip},fork,reuseaddr" \
                    "TCP:127.0.0.1:${port}" || true
            fi
            sleep 1
        done
    ) &
else
    echo "docker-entrypoint: could not determine this container's address; a login's debug port will only be reachable from inside the container (docker exec)" >&2
fi

exec k-ruoka-mcp "$@"
