# 自建 iroh Relay

运行时没有配置自建 Relay 时，默认使用 n0（iroh 的开发方）的公共 Relay 和 DNS 地址
发现：只凭 endpoint id 就能找到运行时，开箱可用。客户端先尝试局域网或公网打洞直连，
直连不可用时才经 Relay 转发端到端加密的流量。Relay 能看到连接元数据和流量大小，
但无法解密 Smelt 的 iroh/QUIC 会话内容。

什么时候值得自建：

- **速度**：公共 Relay 限速。实测 Mac（台湾）到洛杉矶的 Linux，走 n0 公共 Relay 吞吐
  0.19MB/s、控制往返 450ms 以上、刷屏时其他终端的回显会卡住近 10 秒；在那台 Linux 上
  自建 Relay 后吞吐 0.5–1.6MB/s、往返 320ms 左右、回显不受刷屏影响。打洞失败时
  （比如客户端开着 Surge、Clash 的增强模式，见「验证与排障」）流量全走 Relay，
  这时差别最明显。
- **元数据**：不想让第三方看到哪两台设备、什么时候连、流量多大。

自建 Relay 与公共 Relay 一样不要求访问令牌。知道地址的其他 iroh 客户端也能用它转发，
因此部署者需要自行监控带宽和连接数，并通过云平台流量告警控制滥用风险。Smelt 的业务
访问由设备配对控制（一次性配对码 + 运行时里的已配对设备白名单），Relay 开放不会让
会话被匿名访问。

一键部署脚本默认使用 iroh 官方的 `iroh-relay` 1.0.2；仓库锁定的是 iroh 1.x，协议兼容
（与 `iroh-relay` 1.2.0 也实测可用）。脚本只管理 iroh Relay 自己的文件和 systemd 服务，
不会修改 Nginx、WireGuard、UFW 或其他业务。

## 准备

建议使用一台独立的 Ubuntu/Debian 云主机。部署前完成：

1. 将域名的 `A` 记录指向服务器公网 IPv4；使用 IPv6 时再配置 `AAAA`。
2. 在云安全组放行 `TCP 80`、`TCP 443`、`UDP 7842`。
3. 确认服务器的 `80/443 TCP` 和 `7842 UDP` 没有被其他程序占用。

端口用途：

| 端口 | 用途 |
|---|---|
| `80/TCP` | Let's Encrypt HTTP-01 验证及 HTTP 入口 |
| `443/TCP` | Relay HTTPS/WebSocket 数据通道 |
| `7842/UDP` | QUIC 地址发现，帮助双方打洞 |
| `9090/TCP` | Prometheus metrics，仅监听 `127.0.0.1`，无需放行 |

脚本使用 iroh-relay 内置的 Let's Encrypt 客户端签发和续期证书，所以这套一键流程要求
Relay 独占公网 `80/443`，并且必须提供联系邮箱（iroh-relay 的 ACME 客户端强制要求）。
如果服务器已经由 Nginx/Caddy 占用这些端口，脚本会报错退出，不会覆盖现有配置；这种情况
应使用独立主机，或参考后面的「已有反向代理」手动部署。不想留邮箱，见「手动部署：
用 certbot 签证书」。

没有域名时，测试可以用 `sslip.io` 这类把 IP 编进域名的公共解析服务，例如
`216-75-75-181.sslip.io` 解析到 `216.75.75.181`。它是第三方服务，长期使用请换成自己的域名。

## 一键部署

在本机 Smelt 仓库中运行：

```bash
./scripts/deploy-iroh-relay.sh \
  --ssh ubuntu@203.0.113.10 \
  --domain relay.example.com \
  --email admin@example.com
```

脚本会：

- 在本机下载 iroh 官方的 Linux musl 静态二进制并校验 SHA-256；
- 将二进制和安装脚本上传到服务器，因此服务器本身不需要访问 GitHub；
- 创建低权限的 `iroh-relay` 系统用户；
- 安装并启用 `iroh-relay.service`，崩溃后自动重启、开机自动启动；
- 申请 Let's Encrypt 证书并启用 QUIC 地址发现；
- 验证 systemd 状态与公网 TLS。

SSH 使用别名时，`--ssh smelt-relay` 也可以。脚本可重复执行，已有安装会被原子升级并重启。

如果服务器和本机都无法直接下载 GitHub release，可先通过可用网络取得官方
`iroh-relay` Linux 二进制，再指定本地文件：

```bash
./scripts/deploy-iroh-relay.sh \
  --ssh ubuntu@203.0.113.10 \
  --domain relay.example.com \
  --email admin@example.com \
  --binary /path/to/iroh-relay
```

也可以为官方下载地址增加镜像前缀，下载后仍会校验官方 release 的 SHA-256：

```bash
./scripts/deploy-iroh-relay.sh \
  --ssh ubuntu@203.0.113.10 \
  --domain relay.example.com \
  --email admin@example.com \
  --download-prefix https://your-github-proxy.example/
```

直接在服务器执行时，先上传脚本，再运行：

```bash
sudo ./deploy-iroh-relay.sh \
  --domain relay.example.com \
  --email admin@example.com
```

## 手动部署：用 certbot 签证书

不想把邮箱交给 Let's Encrypt，或者想用较新的 iroh-relay 版本时，可以手动部署：用 certbot
免邮箱签证书，iroh-relay 以 `Manual` 模式读证书文件。以下是在一台全新 Debian 13 上的
实际步骤（以 root 执行，域名以 `relay.example.com` 为例）。

1. 编译 iroh-relay（版本与仓库锁定的一致）并签证书：

   ```bash
   apt-get install -y build-essential pkg-config certbot
   curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal && . ~/.cargo/env
   cargo install iroh-relay@1.2.0 --features server --locked
   install -m 755 ~/.cargo/bin/iroh-relay /usr/local/bin/iroh-relay
   certbot certonly --standalone -d relay.example.com \
     --register-unsafely-without-email --agree-tos -n
   ```

2. 写配置 `/etc/iroh-relay.toml`：

   ```toml
   enable_relay = true
   http_bind_addr = "[::]:80"
   enable_quic_addr_discovery = true
   enable_metrics = false

   [tls]
   https_bind_addr = "[::]:443"
   hostname = "relay.example.com"
   cert_mode = "Manual"
   manual_cert_path = "/etc/letsencrypt/live/relay.example.com/fullchain.pem"
   manual_key_path = "/etc/letsencrypt/live/relay.example.com/privkey.pem"
   ```

3. 写 systemd 服务 `/etc/systemd/system/iroh-relay.service`，`ExecStart` 为
   `/usr/local/bin/iroh-relay --config-path /etc/iroh-relay.toml`，`Restart=on-failure`，
   然后 `systemctl enable --now iroh-relay`。

4. 续期钩子：certbot 续期时要占用 `80/TCP`，而 iroh-relay 也监听它，所以续期前先停、
   续完再启动（`Manual` 模式只在启动时读证书，重启正好加载新证书）：

   ```bash
   mkdir -p /etc/letsencrypt/renewal-hooks/{pre,post}
   printf '#!/bin/sh\nsystemctl stop iroh-relay\n' > /etc/letsencrypt/renewal-hooks/pre/iroh-relay.sh
   printf '#!/bin/sh\nsystemctl start iroh-relay\n' > /etc/letsencrypt/renewal-hooks/post/iroh-relay.sh
   chmod 755 /etc/letsencrypt/renewal-hooks/{pre,post}/iroh-relay.sh
   certbot renew --dry-run
   ```

   演练成功、且演练后 `systemctl is-active iroh-relay` 仍是 `active` 即可。续期期间
   Relay 会中断几秒。

## 配置 Smelt

Relay 配置属于**运行时**：每个运行时自己决定用哪个 Relay，生成的配对串会带上
`relay=` 参数，客户端照它去连。

- **Mac 本机运行时**：桌面「设置 → 远程访问」里填 Relay 地址（如 `relay.example.com`，
  省略协议时自动补 `https://`）并打开远程访问。
- **Linux 无头运行时**：目前还没有命令行入口，只能通过控制接口
  `remote.config.set {"enabled": true, "iroh_relay": "https://relay.example.com"}` 设置
  （待补一个 `smeltd` 子命令）。设好后用 `remote.status` 确认 `relay` 字段已经是新地址。

换 Relay 之后，运行时不再通过 n0 的 DNS 公布地址，**已有的配对会连不上**：在客户端
移除这个运行时，再用 `smeltd pair` 或设置页生成的新配对串重新添加。

## 验证与排障

服务器侧：

```bash
sudo systemctl status iroh-relay --no-pager
sudo journalctl -u iroh-relay -f
sudo ss -lntup | grep -E ':(80|443|7842|9090)\b'
curl -I https://relay.example.com/
```

客户端连上后，看运行时的 `~/.smelt/daemon.log` 确认实际路径：

```text
iroh path remote=<设备 id> kind=relay address=relay:https://relay.example.com/ rtt_ms=288
iroh path remote=<设备 id> kind=direct address=ip:203.0.113.20:22016 rtt_ms=168
```

`kind=direct` 是打洞直连，`kind=relay` 是经 Relay 中继；路径变化时会再记一行。

常见问题：

| 现象 | 检查 |
|---|---|
| 证书签发失败 | DNS 是否已经指向服务器；`80/TCP` 是否同时在安全组和系统防火墙放行 |
| HTTPS 可访问但一直不能打洞 | `7842/UDP` 是否放行；不放行仍可能中继，但直连成功率会下降 |
| 服务反复重启 | `journalctl -u iroh-relay -n 100 --no-pager`；检查端口占用和配置格式 |
| 腾讯云无法访问 GitHub | 使用 `--ssh`，二进制在本机下载后上传；或使用 `--binary` |
| 一直 `kind=relay`，或者 `kind=direct` 却很慢 | 客户端是否开着 Surge、Clash 的增强模式（TUN）。它接管 UDP 后客户端变成对称 NAT，打洞基本失败；有时 iroh 会打通一条经代理节点转发的「直连」，延迟低但带宽极差（iroh 按延迟选路，不看带宽）。给 Smelt 设 UDP 直连规则，或把它排除出增强模式 |

## 升级

升级脚本内置版本时，再次执行相同命令即可原子替换二进制并重启服务。部署其他版本必须同时
提供官方 SHA-256，避免镜像或下载链路篡改：

```bash
./scripts/deploy-iroh-relay.sh \
  --ssh ubuntu@203.0.113.10 \
  --domain relay.example.com \
  --email admin@example.com \
  --version 1.0.3 \
  --sha256 <official-linux-musl-sha256>
```

## 已有反向代理

如果 `80/443` 已由 Nginx 或 Caddy 使用，不要强行运行一键脚本。可让 iroh-relay 使用
`Reloading` 证书模式，在回环地址监听 HTTPS，再由现有反向代理转发 WebSocket：

```toml
enable_relay = true
http_bind_addr = "127.0.0.1:3340"
enable_quic_addr_discovery = true
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9090"

[tls]
https_bind_addr = "127.0.0.1:8443"
quic_bind_addr = "0.0.0.0:7842"
hostname = ["relay.example.com"]
cert_mode = "Reloading"
manual_cert_path = "/etc/iroh-relay/tls/fullchain.pem"
manual_key_path = "/etc/iroh-relay/tls/privkey.pem"
```

反向代理必须保留 WebSocket Upgrade，并将 `443/TCP` 转发到 `127.0.0.1:8443`；
`7842/UDP` 仍由 iroh-relay 直接监听，不能经普通 HTTP 反向代理。证书续期后要以原子方式
更新上述两个文件，iroh-relay 会周期性重新加载。由于每台服务器的证书来源和现有站点不同，
脚本不会自动修改这一模式。

## 文件与卸载边界

安装产生：

```text
/usr/local/bin/iroh-relay
/etc/iroh-relay/iroh-relay.toml
/var/lib/iroh-relay/certs/
/etc/systemd/system/iroh-relay.service
```

手动卸载只需停止并禁用 `iroh-relay.service`，再删除以上路径和 `iroh-relay` 系统用户。
不要删除服务器上无关的 Nginx、WireGuard、coturn 或其他服务。

上游参考：

- [iroh-relay README](https://github.com/n0-computer/iroh/blob/v1.0.2/iroh-relay/README.md)
- [iroh v1.0.2 release](https://github.com/n0-computer/iroh/releases/tag/v1.0.2)
