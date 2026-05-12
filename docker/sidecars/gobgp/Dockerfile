FROM golang:1.22-alpine AS gobgp-build
RUN apk add --no-cache git && \
    go install github.com/osrg/gobgp/v3/cmd/gobgpd@latest && \
    go install github.com/osrg/gobgp/v3/cmd/gobgp@latest

FROM python:3.11-alpine

# gobgp daemon + CLI
COPY --from=gobgp-build /go/bin/gobgpd /usr/local/bin/gobgpd
COPY --from=gobgp-build /go/bin/gobgp   /usr/local/bin/gobgp

# Bridge script dependencies
RUN pip install --no-cache-dir grpcio grpcio-tools protobuf

WORKDIR /app
COPY docker/sidecars/gobgp/bgp_ls_bridge.py /app/bgp_ls_bridge.py
COPY docker/sidecars/gobgp/entrypoint.sh    /app/entrypoint.sh
COPY docker/sidecars/gobgp/gobgp.toml       /app/gobgp.toml.template

RUN chmod +x /app/entrypoint.sh

# gobgpd gRPC: 50052  |  BGP: 179
EXPOSE 50052 179

ENTRYPOINT ["/app/entrypoint.sh"]
