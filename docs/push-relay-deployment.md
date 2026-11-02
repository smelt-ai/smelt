# 部署推送中继

iOS 把 App 挂起后，手机和各台机器之间的 iroh 连接就断了。等批准、等输入、运行失败这些
提醒要送到锁屏上，只能走 Apple 的推送服务（APNs）。APNs 只认持有开发者 `.p8` 私钥的
发送方，这把私钥不能随开源的 smeltd 分发，所以由推送中继 `smelt-push-relay` 代发：

```text
手机 ──令牌──▶ 中继 /v1/register ──句柄──▶ 手机 ──push.register（iroh）──▶ 各台 smeltd
smeltd ──句柄 + 提醒类型 + 会话 id──▶ 中继 /v1/send ──签名──▶ APNs ──▶ 手机
```

- 中继不存设备表。它用自己的**句柄密钥**把 APNs 令牌加密成句柄交给手机，smeltd 发推送时
  只带句柄。拿到裸令牌的人没法借这个中继给别人的手机发推送；smeltd 库里也没有裸令牌。
- 推送里没有会话内容：只有提醒类型（手机按 `Localizable.strings` 显示本地化文案）和
  不透明的 `runtime_id`、`session_id`（点开后跳到对应会话）。会话内容仍然只走 iroh。
- 中继按设备令牌限流（连着 10 条，之后每分钟 6 条），日志里不出现令牌、句柄和提供方令牌。
- 中继只给配置里允许的 Bundle ID 推送，而且这些 App 必须和 `.p8` 属于同一个 Team。所以
  **一个 App 构建对应一个中继**：App Store / TestFlight 版用官方中继；自己用开发者账号签名
  的版本，要用自己 Team 的 `.p8` 部署一个中继。

推送判定、延迟窗口和重试见 [`notification-architecture.md`](notification-architecture.md)
的「手机推送」。

## Apple 后台

1. 在 [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/identifiers/list)
   里给 App 的 Bundle ID 打开 **Push Notifications**。Xcode 自动签名带着
   `aps-environment` 的 entitlement 时一般会自己打开，打不开就手动开。
2. 在 **Keys** 里新建一把 key，勾选 **Apple Push Notifications service (APNs)**，下载
   `AuthKey_<KEY_ID>.p8`。这个文件只能下载一次，妥善保存。记下 Key ID 和 Team ID。

同一把 key 对开发版（sandbox）和正式版（production）都能用，也能给同一 Team 下的多个
Bundle ID 推送。

## 部署中继

中继只监听本机，HTTPS 交给前面的反向代理。以下在一台 Debian / Ubuntu 上以 root 执行，
域名以 `push.example.com` 为例。中继在服务器上原生编译（仓库不做交叉编译）。

1. 编译：

   ```bash
   apt-get install -y build-essential pkg-config git
   curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal && . ~/.cargo/env
   git clone https://github.com/smelt-ai/smelt.git && cd smelt
   cargo build --release --locked -p smelt-push-relay
   install -m 755 target/release/smelt-push-relay /usr/local/bin/smelt-push-relay
   ```

2. 放好密钥。句柄密钥是 32 字节随机数；换掉它，所有手机的句柄都会作废（smeltd 发送时收到
   `410` 删掉登记，手机下次打开 App 时重新登记）。

   ```bash
   useradd --system --no-create-home --shell /usr/sbin/nologin smelt-push
   install -d -m 750 -o root -g smelt-push /etc/smelt-push
   install -m 640 -o root -g smelt-push AuthKey_ABC123DEFG.p8 /etc/smelt-push/apns.p8
   openssl rand -base64 32 > /etc/smelt-push/handle.key
   chown root:smelt-push /etc/smelt-push/handle.key && chmod 640 /etc/smelt-push/handle.key
   ```

3. 写配置 `/etc/smelt-push/env`：

   ```bash
   SMELT_PUSH_LISTEN=127.0.0.1:8787
   SMELT_PUSH_APNS_KEY=/etc/smelt-push/apns.p8
   SMELT_PUSH_APNS_KEY_ID=ABC123DEFG
   SMELT_PUSH_TEAM_ID=H3H5ULV8KR
   # 允许推送的 Bundle ID，逗号分隔。同一 Team 下的开发版可以一起列上。
   SMELT_PUSH_TOPICS=ai.smelt.mobile
   SMELT_PUSH_HANDLE_KEY=/etc/smelt-push/handle.key
   ```

4. 写 systemd 服务 `/etc/systemd/system/smelt-push-relay.service`：

   ```ini
   [Unit]
   Description=Smelt push relay
   After=network-online.target
   Wants=network-online.target

   [Service]
   User=smelt-push
   EnvironmentFile=/etc/smelt-push/env
   ExecStart=/usr/local/bin/smelt-push-relay
   Restart=on-failure
   NoNewPrivileges=yes
   ProtectSystem=strict
   ProtectHome=yes
   PrivateTmp=yes

   [Install]
   WantedBy=multi-user.target
   ```

   然后 `systemctl daemon-reload && systemctl enable --now smelt-push-relay`。

5. 反向代理。以 Caddy 为例（自动签证书），`/etc/caddy/Caddyfile`：

   ```text
   push.example.com {
       reverse_proxy 127.0.0.1:8787
   }
   ```

   用 Nginx 也一样，只要把 HTTPS 转到 `127.0.0.1:8787`。

6. 验证：`curl https://push.example.com/healthz` 应该返回 `ok`。服务器还要能访问
   `api.push.apple.com:443` 和 `api.sandbox.push.apple.com:443`。

## 让 App 用这个中继

中继地址编译进 App（Info.plist 的 `SmeltPushRelayHost`），只写主机名，App 一律用 https：

- 正式版：`mobile/ios/Flutter/SigningDefaults.xcconfig` 的 `SMELT_PUSH_RELAY_HOST`。
- 自己签名的开发版：在 `mobile/ios/Flutter/Developer.xcconfig` 里加
  `SMELT_PUSH_RELAY_HOST = push.example.com`，并把自己的 Bundle ID 加进中继的
  `SMELT_PUSH_TOPICS`。

没配中继的构建，设置里的推送开关显示「这个版本没有配置推送服务」。

手机登记时把中继地址告诉每台 smeltd，smeltd 往这个地址发；smeltd 不需要任何配置，只要
能访问中继。smeltd 只接受 `https://` 的中继地址（本机回环地址可以用 `http://`，方便调试）。

## 排障

- 设置里显示「连不上推送服务」：手机访问不到中继的 `/v1/register`。先在手机浏览器里打开
  `https://push.example.com/healthz`。
- 开着推送却收不到：看中继日志（`journalctl -u smelt-push-relay`）。`APNs 拒绝：
  InvalidProviderToken` 是 Key ID / Team ID 不对；`TopicDisallowed` 是这把 key 不属于
  App 所在的 Team；`BadDeviceToken` 多半是开发版的令牌发到了 production（或反过来），检查
  `SMELT_APNS_ENVIRONMENT`。再看 smeltd 日志里以「推送：」开头的行。
- 在电脑前处理掉的审批不会推送：提醒产生后先等 30 秒，这期间处理掉就取消。
