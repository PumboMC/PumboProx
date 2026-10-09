# PumboProx from the static (musl) release binaries. The build context is a
# directory with pumboprox-amd64 and/or pumboprox-arm64, e.g.
#   docker buildx build -f Dockerfile --platform linux/amd64,linux/arm64 image/
# (.github/workflows/release.yml does this for every tag). distroless/static
# brings the CA certificates; :nonroot runs as uid 65532.
FROM gcr.io/distroless/static-debian13:nonroot
ARG TARGETARCH
COPY --chmod=755 pumboprox-${TARGETARCH} /usr/local/bin/pumboprox
WORKDIR /data
EXPOSE 25565
ENTRYPOINT ["pumboprox"]
CMD ["run", "/data/pumboprox.yml"]
