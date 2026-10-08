import type { InlineExtension } from "@earendil-works/pi-coding-agent";
import {
	SMELT_BACKGROUND_TASK_EXTENSION_NAME,
	smeltBackgroundTaskExtension,
} from "./background-task.ts";
import smeltContextUsageExtension from "./context-usage.ts";
import {
	SMELT_ELICITATION_EXTENSION_NAME,
	smeltElicitationExtension,
} from "./elicitation.ts";
import smeltPermissionExtension from "./smelt-permission.ts";
import smeltSubagentExtension, { SMELT_SUBAGENT_EXTENSION_NAME } from "./subagent/index.ts";

// Pi appends inline factories after discovered user/project extensions. Stable names make their
// sourceInfo auditable. Permission stays last so host approval sees the final tool arguments.
export const SMELT_EXTENSION_FACTORIES = [
	{ name: SMELT_ELICITATION_EXTENSION_NAME, factory: smeltElicitationExtension, hidden: true },
	{ name: "smelt-context-usage", factory: smeltContextUsageExtension, hidden: true },
	{ name: SMELT_SUBAGENT_EXTENSION_NAME, factory: smeltSubagentExtension, hidden: true },
	{ name: SMELT_BACKGROUND_TASK_EXTENSION_NAME, factory: smeltBackgroundTaskExtension, hidden: true },
	{ name: "smelt-permission", factory: smeltPermissionExtension, hidden: true },
] satisfies InlineExtension[];
