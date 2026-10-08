import type {
	ExtensionContext,
	ExtensionUIDialogOptions,
	RpcExtensionUIRequest,
	RpcExtensionUIResponse,
} from "@earendil-works/pi-coding-agent";

type DialogRequest = Extract<RpcExtensionUIRequest, { method: "confirm" | "select" | "input" | "editor" }>;
type HostContext = Pick<ExtensionContext, "hasUI" | "ui">;
type SendResponse = (response: RpcExtensionUIResponse) => Promise<void>;

function cancel(request: DialogRequest): RpcExtensionUIResponse {
	return { type: "extension_ui_response", id: request.id, cancelled: true };
}

/** Forward a child's RPC dialog through the parent extension context to Smelt's host UI. */
export async function bridgeSubagentUiRequest(
	request: RpcExtensionUIRequest,
	context: HostContext,
	signal: AbortSignal | undefined,
	sendResponse: SendResponse,
): Promise<void> {
	if (request.method !== "confirm" && request.method !== "select" && request.method !== "input" && request.method !== "editor") {
		return;
	}

	const dialog = request as DialogRequest;
	if (!context.hasUI || signal?.aborted) {
		await sendResponse(cancel(dialog));
		return;
	}

	const options: ExtensionUIDialogOptions | undefined =
		request.method !== "editor" && (request.timeout !== undefined || signal)
			? {
					...(request.timeout !== undefined ? { timeout: request.timeout } : {}),
					...(signal ? { signal } : {}),
				}
			: undefined;

	try {
		switch (request.method) {
			case "confirm": {
				const confirmed = await context.ui.confirm(request.title, request.message, options);
				await sendResponse({ type: "extension_ui_response", id: request.id, confirmed });
				break;
			}
			case "select": {
				const value = await context.ui.select(request.title, request.options, options);
				await sendResponse(
					value === undefined
						? cancel(request)
						: { type: "extension_ui_response", id: request.id, value },
				);
				break;
			}
			case "input": {
				const value = await context.ui.input(request.title, request.placeholder, options);
				await sendResponse(
					value === undefined
						? cancel(request)
						: { type: "extension_ui_response", id: request.id, value },
				);
				break;
			}
			case "editor": {
				const value = await context.ui.editor(request.title, request.prefill);
				await sendResponse(
					value === undefined
						? cancel(request)
						: { type: "extension_ui_response", id: request.id, value },
				);
				break;
			}
		}
	} catch {
		await sendResponse(cancel(dialog));
	}
}
