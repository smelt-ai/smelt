// ACP 适配器的命令行入口：编译脚本（scripts/compile-smelt-agent.ts）按各包 package.json
// 的 `bin` 解析这个说明符。Smelt 只执行入口的副作用（启动 ACP 服务），不从模块读取 API。
declare module "smelt-adapter-bin:*";
