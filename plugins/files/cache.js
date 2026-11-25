// 打开过的文件内容的有界内存 LRU：同一台机器的同一个路径再打开时先秒开，再带版本问运行时
// 变没变（`file.open` 的 `if_none_match`），没变就不重传正文。

/** 缓存总量上限（按正文字符数 / base64 长度算，约 32 MB）。超过时淘汰最久没用的。 */
export const MAX_CACHE_BYTES = 32 * 1024 * 1024;

/**
 * @typedef {{ kind: "text", text: string, version: string }
 *   | { kind: "image", dataUri: string, version: string | null }} CachedFileContent
 */

/** @param {CachedFileContent} value */
function byteLen(value) {
  return value.kind === "text" ? value.text.length : value.dataUri.length;
}

/** @param {string | null} machine @param {string} path */
function cacheKey(machine, path) {
  return `${machine ?? ""}\0${path}`;
}

export class FileContentCache {
  constructor(maxBytes = MAX_CACHE_BYTES) {
    this.maxBytes = maxBytes;
    this.totalBytes = 0;
    /** Map 按插入顺序迭代，最久没用的在最前。@type {Map<string, { machine: string | null, path: string, value: CachedFileContent, bytes: number }>} */
    this.entries = new Map();
  }

  /** @param {string | null} machine @param {string} path @returns {CachedFileContent | null} */
  get(machine, path) {
    const key = cacheKey(machine, path);
    const entry = this.entries.get(key);
    if (!entry) return null;
    this.entries.delete(key);
    this.entries.set(key, entry);
    return entry.value;
  }

  /** 单条超过上限一半的不缓存（顺带清掉旧的那条）。@param {string | null} machine @param {string} path @param {CachedFileContent} value */
  put(machine, path, value) {
    this.remove(machine, path);
    const bytes = byteLen(value);
    if (bytes > this.maxBytes / 2) return;
    this.entries.set(cacheKey(machine, path), { machine, path, value, bytes });
    this.totalBytes += bytes;
    for (const [key, entry] of this.entries) {
      if (this.totalBytes <= this.maxBytes) break;
      this.entries.delete(key);
      this.totalBytes -= entry.bytes;
    }
  }

  /** @param {string | null} machine @param {string} path */
  remove(machine, path) {
    const key = cacheKey(machine, path);
    const entry = this.entries.get(key);
    if (!entry) return;
    this.entries.delete(key);
    this.totalBytes -= entry.bytes;
  }

  /**
   * 删掉这台机器上 `prefix` 这个目录（或文件）底下的全部条目。
   * @param {string | null} machine
   * @param {string} prefix
   * @param {(path: string, base: string) => boolean} startsWith 那台机器的路径规则
   */
  removeUnder(machine, prefix, startsWith) {
    for (const [key, entry] of this.entries) {
      if (entry.machine === machine && startsWith(entry.path, prefix)) {
        this.entries.delete(key);
        this.totalBytes -= entry.bytes;
      }
    }
  }
}
