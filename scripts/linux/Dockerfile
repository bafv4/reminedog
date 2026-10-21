# reminedog の Linux のビルドとスモークテストの環境（開発用）：JDK、Rust、Xvfb、Mesa（llvmpipe）。
# エージェントと LWJGL のスモークのテスト用プログラム（ci/smoke）を画面なしで動かす。
# scripts/linux/run.sh がビルドして使う。リポジトリは /src にマウントする。
FROM eclipse-temurin:25-jdk-noble

ARG RUST_TOOLCHAIN=1.99.0

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential pkg-config curl ca-certificates git \
        xvfb xauth mesa-utils x11-utils xdotool \
        libgl1 libglx-mesa0 libgl1-mesa-dri libegl1 libegl-mesa0 \
        libx11-6 libxrandr2 libxinerama1 libxcursor1 libxi6 libxext6 libxxf86vm1 \
        libxkbcommon0 libwayland-client0 libwayland-egl1 libdecor-0-0 \
        fontconfig fonts-ipafont-gothic \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain "$RUST_TOOLCHAIN" -c clippy,rustfmt

WORKDIR /src
