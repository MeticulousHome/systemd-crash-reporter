#!/bin/bash
DIR=$(dirname $0)
PROJECT="$(pwd)/${DIR}"

# Run to register qemu static
#docker run --rm --privileged multiarch/qemu-user-static --reset -p yes

docker run --rm  \
    --platform=linux/arm64/v8 \
    -v ${PROJECT}:/app \
    ${CACHE_OPTIONS} \
    -w /app \
    arm64v8/rust \
    bash -c "apt update && apt -y install libssl-dev && cargo build --release"
