#!/usr/bin/env bash
# 与 Rust 的 std::env::consts::{OS,ARCH} 对齐的目标名。
# macOS uname -m 是 arm64，Rust 是 aarch64。
runtime_target() {
  case "$(uname -s)" in
    Darwin) RUNTIME_OS=macos ;;
    Linux) RUNTIME_OS=linux ;;
    *)
      echo "不支持的系统：$(uname -s)" >&2
      return 1
      ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) RUNTIME_ARCH=aarch64 ;;
    x86_64) RUNTIME_ARCH=x86_64 ;;
    *)
      echo "不支持的架构：$(uname -m)" >&2
      return 1
      ;;
  esac
}
