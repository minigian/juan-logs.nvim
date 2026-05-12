#!/bin/bash

# Not even here

cd "$(dirname "$0")/../.."

echo "Building 32bit docker image..."
docker build -t juanlog-32bit -f tools/32bit-test/Dockerfile tools/32bit-test/

echo "Running 32bit docker container..."

docker run -it --rm \
    -v "$(pwd):/juan-logs" \
    -v juan-logs-32bit-target:/juan-logs/target \
    juanlog-32bit