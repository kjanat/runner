# Binary-only image: `COPY --from` source for other builds, not a runtime.
# Context is laid out by .github/scripts/docker.sh prepare.
FROM scratch

ARG TARGETARCH

COPY --chmod=0755 ${TARGETARCH}/runner /runner
COPY --chmod=0755 ${TARGETARCH}/run /run

ENTRYPOINT ["/runner"]
