# egm-team, egm-dsh and egm-audit in one image. The worker runs egm-dsh, which runs the coding
# agent: DeepSeek Harness (`dsh`, a Node package) and bubblewrap for its sandbox.
FROM rust:1.99-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p egm-dsh -p egm-web -p egm-audit

FROM node:22-bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends git bubblewrap python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @deepseek-ai/dsh@latest \
    && useradd --create-home --uid 10001 egm
COPY --from=build /src/target/release/egm-team /src/target/release/egm-dsh /src/target/release/egm-audit /usr/local/bin/
USER egm
WORKDIR /home/egm
ENTRYPOINT ["egm-team", "--config", "/etc/egm-team/egm-team.toml"]
CMD ["serve"]
