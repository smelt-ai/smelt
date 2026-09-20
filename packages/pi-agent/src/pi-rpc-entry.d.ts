// 上游发布包包含 RPC 声明文件，但 package exports 尚未给该子路径声明 `types`。
// Smelt 只需要执行它的入口副作用，不从该模块读取 API。
declare module "@earendil-works/pi-coding-agent/rpc-entry";
