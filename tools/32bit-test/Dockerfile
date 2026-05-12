# Here are no funny comments. What are you looking for?

FROM i386/debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y \
    neovim \
    curl \
    build-essential \
    git \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-host i686-unknown-linux-gnu

ENV PATH="/root/.cargo/bin:${PATH}"

RUN echo 'for i in $(seq 1 5000000); do echo "Log line $i: The quick brown fox jumps over the lazy dog in a 32-bit world[ERROR: $(date +%s)]"; done > /root/huge_log_500mb.txt' > /root/gen_log.sh && chmod +x /root/gen_log.sh

WORKDIR /juan-logs

CMD ["/bin/bash", "-c", "cargo build && /bin/bash"]