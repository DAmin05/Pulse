#!/usr/bin/env bash
# Generates Python protobuf + gRPC code into src/pulse/v1 (gitignored).
set -euo pipefail
cd "$(dirname "$0")/.."

out=src
rm -rf "$out/pulse"
python -m grpc_tools.protoc \
  -I ../proto \
  --python_out="$out" --pyi_out="$out" --grpc_python_out="$out" \
  ../proto/pulse/v1/*.proto
touch "$out/pulse/__init__.py" "$out/pulse/v1/__init__.py"
echo "generated $(ls "$out"/pulse/v1/*_pb2*.py | wc -l | tr -d ' ') modules into $out/pulse/v1"
