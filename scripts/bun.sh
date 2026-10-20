#!/usr/bin/env bash
# 打印钉死版本的 Bun。不回落到 PATH。
exec "$(cd "$(dirname "$0")" && pwd)/fetch-bun.sh" "$@"
