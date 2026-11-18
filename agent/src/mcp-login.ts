/**
 * Pure helpers for `mcp-main.ts login`: the sign-in runs on the runtime machine, the user may be
 * approving it in a browser on another device.
 */

import type { LoginNote } from "./auth-protocol.ts";

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
export function callbackTarget(
	authorizationUrl: string,
	pasted: string,
): { url: URL } | { error: string; note: LoginNote } {
	const redirect = redirectUriOf(authorizationUrl);
	if (!redirect) {
		return {
			error:
				"The authorization link has no callback address, so it can't be passed along. Authorize in a browser on this machine.",
			note: { kind: "no_redirect" },
		};
	}
	let url: URL;
	try {
		url = new URL(pasted.trim());
	} catch {
		return {
			error: "That isn't a full address. Copy the whole thing from the browser's address bar (it starts with http://).",
			note: { kind: "not_full_address" },
		};
	}
	if (url.origin !== redirect.origin || url.pathname !== redirect.pathname) {
		const expected = `${redirect.origin}${redirect.pathname}`;
		return {
			error: `That isn't this sign-in's callback address; it should start with ${expected}`,
			note: { kind: "wrong_callback", expected },
		};
	}
	if (!url.searchParams.has("code") && !url.searchParams.has("error")) {
		return {
			error: "The address has no authorization result (code). Approve on the authorization page first, then copy it.",
			note: { kind: "no_authorization_result" },
		};
	}
	return { url };
}
