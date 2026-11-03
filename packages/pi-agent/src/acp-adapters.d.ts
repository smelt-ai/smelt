// 这两个入口的 package exports 没有给 TypeScript 可用的声明。
// Smelt 只执行入口的副作用（启动 ACP 服务），不从模块读取 API。
declare module "@agentclientprotocol/claude-agent-acp/dist/index.js";
declare module "@agentclientprotocol/codex-acp/dist/index.js";
