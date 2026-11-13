/**
 * Pure helpers for `mcp-main.ts login`: the sign-in runs on the runtime machine, the user may be
 * approving it in a browser on another device.
 */

/** Where Pi's loopback callback of this sign-in listens, taken from its authorization URL. */
export function redirectUriOf(authorizationUrl: string): URL | undefined {
	try {
		const redirect = new URL(authorizationUrl).searchParams.get("redirect_uri");
		return redirect ? new URL(redirect) : undefined;
	} catch {
		return undefined;
	}
}

/**
 * The URL to request on this machine so that a redirect pasted by the user reaches Pi's loopback
 * callback, as if the browser had reached it.
 *
 * A browser on another device (phone, the user's laptop with a remote runtime) is redirected to its
 * own 127.0.0.1 and fails there. The address bar then holds `code` and `state`, and delivering them
 * to the callback on this machine finishes the sign-in; Pi's callback checks `state` itself.
 *
 * Only this sign-in's redirect URI is accepted: anything else would make the helper fetch an
 * arbitrary URL the user pasted.
 */
export function callbackTarget(authorizationUrl: string, pasted: string): { url: URL } | { error: string } {
	const redirect = redirectUriOf(authorizationUrl);
	if (!redirect) return { error: "授权链接里没有回调地址，没法转交，请在这台机器的浏览器里授权" };
	let url: URL;
	try {
		url = new URL(pasted.trim());
	} catch {
		return { error: "这不是完整的地址，请从浏览器地址栏整段复制（以 http:// 开头）" };
	}
	if (url.origin !== redirect.origin || url.pathname !== redirect.pathname) {
		return { error: `这不是这次登录的回调地址，应以 ${redirect.origin}${redirect.pathname} 开头` };
	}
	if (!url.searchParams.has("code") && !url.searchParams.has("error")) {
		return { error: "地址里没有授权结果（code），请在授权页点完同意后再复制" };
	}
	return { url };
}
