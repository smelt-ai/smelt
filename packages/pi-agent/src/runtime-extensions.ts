import type { ExtensionFactory } from "@earendil-works/pi-coding-agent";
import smeltContextUsageExtension from "./context-usage.ts";
import smeltElicitationExtension from "./elicitation.ts";
import smeltPermissionExtension from "./smelt-permission.ts";

// Pi appends inline factories after discovered user/project extensions. Permission stays last so
// host approval sees the final tool arguments. Elicitation is a host conversation primitive, not a
// user plugin: models get a choice card without the user writing an extension.
export const SMELT_EXTENSION_FACTORIES = [
	smeltElicitationExtension,
	smeltContextUsageExtension,
	smeltPermissionExtension,
] satisfies ExtensionFactory[];
