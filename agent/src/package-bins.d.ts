// npm 包的命令行入口（ACP 适配器、MCP 服务器）：编译脚本（scripts/compile-smelt-agent.ts）按各包
// package.json 的 `bin` 解析这个说明符。Smelt 只执行入口的副作用（启动服务），不从模块读取 API。
declare module "smelt-bin:*";
