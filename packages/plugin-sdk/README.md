# @smelt-ai/plugin-sdk

Contracts for Bun modules loaded by Smelt's shared Bun Host. Bun has the current
user's OS permissions and is not a sandbox. User-installed packages run the same way.

Every Bun plugin manifest explicitly declares:

```json
{
  "entrypoint": "bin/main.ts"
}
```

Its entrypoint default-exports a `SharedPlugin`:

```ts
import {
  InvocationFailure,
  type InvocationRequest,
  type SharedPlugin,
} from "@smelt-ai/plugin-sdk";

const plugin: SharedPlugin = {
  async invoke(request: InvocationRequest, context) {
    if (request.operation !== "echo") {
      throw new InvocationFailure("invalid_request", "unknown operation");
    }
    return { plugin_id: context.pluginId, payload: request.payload };
  },
};

export default plugin;
```

`context.dataDir` is a host-derived per-package data directory. Shared modules do not receive
a daemon socket, credential, event subscription channel, or direct action channel. The host
validates manifest declarations and dispatches only declared contributions.
