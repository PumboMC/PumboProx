# PumboProx from the static (musl) release binaries. The build context is a
# directory with pumboprox-amd64 and/or pumboprox-arm64 and the license files
# of a release archive (LICENSE, NOTICE, licenses/, THIRD-PARTY-LICENSES.txt), e.g.
#   docker buildx build -f Dockerfile --platform linux/amd64,linux/arm64 image/
# (.github/workflows/release.yml does this for every tag). distroless/static
# brings the CA certificates; :nonroot runs as uid 65532.
FROM gcr.io/distroless/static-debian13:nonroot
ARG TARGETARCH
COPY --chmod=755 pumboprox-${TARGETARCH} /usr/local/bin/pumboprox
COPY LICENSE NOTICE THIRD-PARTY-LICENSES.txt /usr/share/doc/pumboprox/
COPY licenses /usr/share/doc/pumboprox/licenses
WORKDIR /data
EXPOSE 25565
# The proxy stops its managed servers on SIGTERM (Docker's default) and logs
# how long that may take: give the container a stop_grace_period of at least
# that (60s with the default stop-timeout-secs), and init: true so servers
# it starts are reaped.
STOPSIGNAL SIGTERM
ENTRYPOINT ["pumboprox"]
CMD ["run", "/data/pumboprox.yml"]
