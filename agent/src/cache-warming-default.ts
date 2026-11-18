import { type CacheWarmingMode, SettingsManager } from "@earendil-works/pi-coding-agent";

/**
 * Smelt 里提示缓存只在一轮进行中续热，两轮之间空着就让它过期。
 *
 * Pi 1.5.1 把默认值从 `"streaming"` 改成了 `"idle"`：空闲时也会按它的收益估算续热最多 30 分钟。
 * Smelt 同时挂着很多会话，大多数停下以后不会很快再问，续热的钱多半白花。用户在 Pi 设置里
 * 明确写了 `cacheWarming` 的，照用户的来；没写才用这里的默认值。
 */
export const SMELT_CACHE_WARMING_DEFAULT: CacheWarmingMode = "streaming";

let installed = false;

/** Pi 没有改默认值的入口，只能在 Smelt 的入口里改写读取模式的那个方法。 */
export function installCacheWarmingDefault(): void {
	if (installed) return;
	installed = true;
	const proto = SettingsManager.prototype;
	const original = proto.getCacheWarmingMode;
	proto.getCacheWarmingMode = function () {
		if (this.getGlobalSettings().cacheWarming === undefined) return SMELT_CACHE_WARMING_DEFAULT;
		return original.call(this);
	};
}
