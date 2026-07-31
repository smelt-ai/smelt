# 自建 iroh Relay

Smelt 不内置公共 Relay。手机与 Mac 会优先尝试局域网或公网打洞直连；直连不可用时，
才通过用户配置的 iroh Relay 转发端到端加密流量。Relay 能看到连接元数据和流量大小，
但无法解密 Smelt 的 iroh/QUIC 会话内容。

这套配置与 iroh 公共 Relay 一样不要求共享访问令牌。知道 Relay 地址的其他 iroh 客户端
也可以使用它，因此部署者需要自行监控带宽和连接数，并通过云平台流量告警控制滥用风险。
Smelt 的业务访问仍由配对码里的网关 token 鉴权，不会因为 Relay 开放而匿名暴露会话。

本文使用 iroh 官方的 `iroh-relay` 1.0.2，与仓库当前锁定的 iroh 1.x 协议版本一致。
部署脚本只管理 iroh Relay 自己的文件和 systemd 服务，不会修改 Nginx、WireGuard、
UFW 或其他业务。

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
Relay 独占公网 `80/443`。如果服务器已经由 Nginx/Caddy 占用这些端口，脚本会报错退出，
不会覆盖现有配置；这种情况应使用独立主机，或参考后面的「已有反向代理」手动部署。

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

## 配置 Smelt

部署完成后，在 Mac 的「设置 → 远程」中填写：

- Relay 地址：`relay.example.com`，省略协议时 Smelt 自动使用 `https://`；
- 开启远程：打开。

如果 Relay 地址在远程开启期间发生变化，点击分享卡片中的「重试」生成包含新配置的
配对二维码，然后让手机重新扫码。

## 验证与排障

服务器侧：

```bash
sudo systemctl status iroh-relay --no-pager
sudo journalctl -u iroh-relay -f
sudo ss -lntup | grep -E ':(80|443|7842|9090)\b'
curl -I https://relay.example.com/
```

Mac 和手机连接后，移动端顶部应显示当前路径：

- `LAN`：同一局域网直连；
- `P2P`：打洞后的公网直连；
- `Relay`：直连失败，正在通过自建 Relay 中继。

常见问题：

| 现象 | 检查 |
|---|---|
| 证书签发失败 | DNS 是否已经指向服务器；`80/TCP` 是否同时在安全组和系统防火墙放行 |
| HTTPS 可访问但一直不能打洞 | `7842/UDP` 是否放行；不放行仍可能中继，但直连成功率会下降 |
| 服务反复重启 | `journalctl -u iroh-relay -n 100 --no-pager`；检查端口占用和配置格式 |
| 腾讯云无法访问 GitHub | 使用 `--ssh`，二进制在本机下载后上传；或使用 `--binary` |

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
