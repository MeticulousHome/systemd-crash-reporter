FROM debian:bookworm

# Install the curl and build-essential packages
RUN apt-get update && \
    apt-get install -y curl build-essential && \
    apt-get clean
    
#Create volumen to mount the context dir in
VOLUME ["/systemd-crash-reporter"]

# Install Rust using rustup
# RUN curl  --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

CMD ["/systemd-crash-reporter/build.sh"]

