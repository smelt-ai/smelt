// 对话基准用的假 ACP agent（stdio JSON-RPC），不调任何模型。
//
// prompt 里写 `stream <片段数> <每片字节> <间隔毫秒> <标签>`，就按这个节奏流式吐出助手
// 回复，最后一片是 `__END_<标签>__`，然后结束回合。标签用来区分各个回合。其余请求一律回空结果。
//
// 由运行时的 smelt-agent 当 Bun 执行（见 remote_terminal_bench.rs 的 `bench_agent_launch`），
// 整段包在 async 函数里：`-e` 拿到的是 eval 的代码，顶层不能 `for await`。
(async () => {
  const readline = require("node:readline");

  // 等写进管道再继续，片段间隔才是真实的节奏，不会被缓冲攒成一大块。
  const send = (message) =>
    new Promise((resolve) => process.stdout.write(JSON.stringify(message) + "\n", resolve));
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  const chunk = (sessionId, text) =>
    send({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId,
        update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text } },
      },
    });

  for await (const raw of readline.createInterface({ input: process.stdin })) {
    const line = raw.trim();
    if (!line) continue;
    const message = JSON.parse(line);
    if (!("id" in message) || !("method" in message)) continue;
    const params = message.params || {};
    let result = {};
    if (message.method === "initialize") {
      result = { protocolVersion: 1, agentCapabilities: {} };
    } else if (message.method === "session/new") {
      // 每个进程一个会话 id：Smelt 不许两个会话共用同一个 provider 会话。
      result = { sessionId: `bench-${crypto.randomUUID().replaceAll("-", "")}` };
    } else if (message.method === "session/prompt") {
      const text = (params.prompt || []).map((part) => part.text || "").join(" ");
      const found = /stream (\d+) (\d+) (\d+) (\S+)/.exec(text);
      const [count, size, delay, tag] = found
        ? [Number(found[1]), Number(found[2]), Number(found[3]), found[4]]
        : [1, 16, 0, "none"];
      for (let index = 0; index < count; index++) {
        const body = `[${tag} ${String(index).padStart(6, "0")}] ` + "x".repeat(Math.max(size - 10, 0)) + "\n";
        await chunk(params.sessionId, body);
        if (delay) await sleep(delay);
      }
      await chunk(params.sessionId, `__END_${tag}__\n`);
      result = { stopReason: "end_turn" };
    }
    await send({ jsonrpc: "2.0", id: message.id, result });
  }
})();
