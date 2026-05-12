#!/bin/sh
set -e

GOBGP_AS="${GOBGP_AS:-65200}"
GOBGP_ROUTER_ID="${GOBGP_ROUTER_ID:-192.0.2.254}"
GOBGP_PEER_ADDRESS="${GOBGP_PEER_ADDRESS:-}"
GOBGP_PEER_AS="${GOBGP_PEER_AS:-$GOBGP_AS}"
BONSAI_BGP_LS_ADDR="${BONSAI_BGP_LS_ADDR:-host.docker.internal}"
BONSAI_BGP_LS_PORT="${BONSAI_BGP_LS_PORT:-10179}"

if [ -z "$GOBGP_PEER_ADDRESS" ]; then
    echo "ERROR: GOBGP_PEER_ADDRESS must be set to the BGP-LS peer (RR/PE address)" >&2
    exit 1
fi

# Render config from template
sed \
    -e "s/\${GOBGP_AS}/$GOBGP_AS/g" \
    -e "s/\${GOBGP_ROUTER_ID}/$GOBGP_ROUTER_ID/g" \
    -e "s/\${GOBGP_PEER_ADDRESS}/$GOBGP_PEER_ADDRESS/g" \
    -e "s/\${GOBGP_PEER_AS}/$GOBGP_PEER_AS/g" \
    /app/gobgp.toml.template > /tmp/gobgp.toml

echo "Starting gobgpd (AS=$GOBGP_AS, router-id=$GOBGP_ROUTER_ID, peer=$GOBGP_PEER_ADDRESS)"
gobgpd --config-file /tmp/gobgp.toml --config-type toml --log-plain &
GOBGPD_PID=$!

# Wait for gobgpd gRPC to be ready
retries=0
until gobgp -u localhost:50052 global show 2>/dev/null; do
    retries=$((retries + 1))
    if [ "$retries" -ge 20 ]; then
        echo "ERROR: gobgpd did not become ready after 20s" >&2
        kill "$GOBGPD_PID" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done

echo "gobgpd ready — starting BGP-LS bridge → $BONSAI_BGP_LS_ADDR:$BONSAI_BGP_LS_PORT"
exec python3 /app/bgp_ls_bridge.py \
    --gobgp-addr "localhost:50052" \
    --bonsai-addr "$BONSAI_BGP_LS_ADDR" \
    --bonsai-port "$BONSAI_BGP_LS_PORT"
