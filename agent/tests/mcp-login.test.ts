import { describe, expect, test } from "bun:test";
import { callbackTarget, redirectUriOf } from "../src/mcp-login.ts";

const authorizationUrl =
	"https://www.figma.com/oauth/mcp?response_type=code&client_id=c1" +
	"&redirect_uri=http%3A%2F%2F127.0.0.1%3A33909%2Fcallback&state=s1&code_challenge=x";

describe("mcp login redirect", () => {
	test("the redirect URI comes from the authorization URL", () => {
		expect(redirectUriOf(authorizationUrl)?.href).toBe("http://127.0.0.1:33909/callback");
		expect(redirectUriOf("https://example.com/authorize")).toBeUndefined();
		expect(redirectUriOf("not a url")).toBeUndefined();
	});

	test("a pasted redirect of this sign-in is delivered as pasted", () => {
		const result = callbackTarget(authorizationUrl, "  http://127.0.0.1:33909/callback?code=c9&state=s1\n");
		expect("url" in result && result.url.href).toBe("http://127.0.0.1:33909/callback?code=c9&state=s1");
	});

	test("an error redirect is delivered too, so the sign-in reports it", () => {
		const result = callbackTarget(authorizationUrl, "http://127.0.0.1:33909/callback?error=access_denied&state=s1");
		expect("url" in result).toBe(true);
	});

	test("anything but this sign-in's callback is refused", () => {
		for (const pasted of [
			"http://127.0.0.1:8080/callback?code=c9&state=s1",
			"http://127.0.0.1:33909/other?code=c9&state=s1",
			"http://169.254.169.254/latest/meta-data?code=1",
			"https://evil.example/callback?code=c9",
		]) {
			expect("error" in callbackTarget(authorizationUrl, pasted)).toBe(true);
		}
	});

	test("a partial copy or a page without a result is refused", () => {
		expect("error" in callbackTarget(authorizationUrl, "127.0.0.1:33909/callback?code=c9")).toBe(true);
		expect("error" in callbackTarget(authorizationUrl, "http://127.0.0.1:33909/callback")).toBe(true);
	});
});
