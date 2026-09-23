import type { InlineExtension } from "@earendil-works/pi-coding-agent";
import smeltContextUsageExtension from "./context-usage.ts";
import {
	SMELT_ELICITATION_EXTENSION_NAME,
	smeltElicitationExtension,
} from "./elicitation.ts";
import smeltPermissionExtension from "./smelt-permission.ts";

// Pi appends inline factories after discovered user/project extensions. Stable names make their
// sourceInfo auditable. Permission stays last so host approval sees the final tool arguments.
export const SMELT_EXTENSION_FACTORIES = [
	{ name: SMELT_ELICITATION_EXTENSION_NAME, factory: smeltElicitationExtension, hidden: true },
	{ name: "smelt-context-usage", factory: smeltContextUsageExtension, hidden: true },
	{ name: "smelt-permission", factory: smeltPermissionExtension, hidden: true },
] satisfies InlineExtension[];
