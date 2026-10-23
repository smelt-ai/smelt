// 测试用的 `smelt-fs`：宿主模块（crates/smelt/src/plugins/host_fs.rs）的 Node 实现。
// 不做主目录门控，其余语义照宿主：作用于路径本身的调用不跟随最后一段软链。

import * as fs from "node:fs";

function kindOf(stat) {
  if (stat.isSymbolicLink()) return "symlink";
  if (stat.isDirectory()) return "dir";
  if (stat.isFile()) return "file";
  return "other";
}

export function lstat(path) {
  try {
    const stat = fs.lstatSync(path);
    return { kind: kindOf(stat), mtime_ms: stat.mtimeMs };
  } catch {
    return null;
  }
}

export function readlink(path) {
  try {
    return fs.readlinkSync(path);
  } catch {
    return null;
  }
}

export function realpath(path) {
  try {
    return fs.realpathSync(path);
  } catch {
    return null;
  }
}

export function symlink(target, at) {
  fs.realpathSync(target);
  fs.symlinkSync(target, at);
  return null;
}

export function rename(from, to) {
  if (lstat(to) !== null) throw new Error(`目标已存在：${to}`);
  fs.renameSync(from, to);
  return null;
}

export function unlink(path) {
  if (lstat(path)?.kind === "dir") throw new Error(`${path} 是目录，unlink 只删文件或软链`);
  fs.unlinkSync(path);
  return null;
}

export function exchange() {
  throw new Error("测试里没有用到 exchange");
}

export function copy(source, destination) {
  fs.cpSync(source, destination, { recursive: true });
  return null;
}
