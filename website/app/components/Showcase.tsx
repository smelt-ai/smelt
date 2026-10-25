const STEPS = [
  {
    n: "01",
    title: "把运行时装到 Linux",
    body: "一条命令装上 smeltd。需要远程桌面时加上 --with-desktop：TigerVNC、XFCE、常驻 Chrome，以及三个用户级 systemd 单元。",
    code: "curl -fsSL https://github.com/smelt-ai/smelt/releases/latest/download/install.sh | bash -s -- --with-desktop",
  },
  {
    n: "02",
    title: "配对，并选定中继",
    body: "smeltd pair 打开远程访问，打出一次性配对串。自建 iroh 中继在配对前设好，配对串会带上它。",
    code: "smeltd status\nsmeltd relay https://relay.example.com\nsmeltd pair",
  },
  {
    n: "03",
    title: "接上浏览器，从桌面或手机看",
    body: "playwright 预设会看这台机器有没有远程桌面。有就连 127.0.0.1:9222。Mac 上打开「桌面」页签，或在手机「出菜口」里批那条命令。",
    code: "smeltd mcp add playwright\nsmeltd relay          # 查看当前中继\nsmeltd relay off      # 回到 n0 公共中继",
  },
];

export function Showcase() {
  return (
    <section id="topology" className="mx-auto max-w-6xl scroll-mt-20 px-6 py-20">
      <p className="font-mono text-[11px] tracking-[0.18em] text-accent">架构</p>
      <h2 className="mt-3 max-w-2xl text-3xl font-semibold tracking-tight text-foreground">
        客户端只是窗口，状态在每台机器的 smeltd 里
      </h2>
      <p className="mt-3 max-w-2xl text-sm leading-6 text-muted">
        macOS GPUI 桌面和 iOS / Android「出菜口」都是客户端。本机走 Unix socket，远程走 iroh QUIC。两边都是 Control API v3。
      </p>

      <div className="mt-10 overflow-hidden rounded-lg border border-border">
        <div className="grid gap-px bg-border md:grid-cols-3">
          <Node
            kicker="客户端"
            title="macOS GPUI"
            lines={["桌面工作台", "本机 Unix socket", "~/.smelt/smeltd.sock"]}
          />
          <Node
            kicker="客户端"
            title="iOS / Android"
            lines={["出菜口", "smelt-mobile.sqlite3", "同时连接全部已配对机器"]}
          />
          <Node
            kicker="通道"
            title="Control API v3"
            lines={["本机：Unix socket", "远程：iroh QUIC P2P", "短请求，不取代 ACP 流"]}
          />
        </div>
        <div className="border-t border-border bg-surface px-5 py-5">
          <p className="font-mono text-[11px] text-accent">每台机器上的 smeltd</p>
          <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-5">
            {[
              ["smelt-pi", "JSONL RPC"],
              ["ACP 适配器", "受管 Bun"],
              ["PTY", "真终端"],
              ["noVNC", "远程桌面"],
              ["SQLite", "v24 + 客户端 v4"],
            ].map(([name, detail]) => (
              <div key={name} className="rounded-md border border-border bg-background px-3 py-2">
                <p className="text-sm text-foreground">{name}</p>
                <p className="font-mono text-[11px] text-dim">{detail}</p>
              </div>
            ))}
          </div>
        </div>
      </div>

      <ol className="mt-8 grid gap-3 lg:grid-cols-3">
        {STEPS.map((step) => (
          <li key={step.n} className="rounded-lg border border-border bg-surface p-5">
            <p className="font-mono text-[11px] text-accent">{step.n}</p>
            <h3 className="mt-2 text-base font-semibold text-foreground">{step.title}</h3>
            <p className="mt-2 text-sm leading-6 text-muted">{step.body}</p>
            <pre className="mt-4 overflow-x-auto rounded-md border border-border bg-background p-3 font-mono text-[11px] leading-5 text-foreground">
              {step.code}
            </pre>
          </li>
        ))}
      </ol>
    </section>
  );
}

function Node({
  kicker,
  title,
  lines,
}: {
  kicker: string;
  title: string;
  lines: string[];
}) {
  return (
    <div className="bg-background px-5 py-5">
      <p className="font-mono text-[10px] tracking-widest text-dim">{kicker}</p>
      <h3 className="mt-2 text-base font-semibold text-foreground">{title}</h3>
      <ul className="mt-3 space-y-1 font-mono text-[12px] text-muted">
        {lines.map((line) => (
          <li key={line}>{line}</li>
        ))}
      </ul>
    </div>
  );
}
