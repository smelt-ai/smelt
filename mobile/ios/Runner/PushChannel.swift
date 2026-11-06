import Flutter
import UIKit
import UserNotifications

/// 推送通知的原生一侧（`lib/data/push.dart` 的 `MethodChannelPushPlatform`）：通知权限、
/// APNs 令牌、通知点击、通知中心里已送达的推送。
///
/// 推送载荷的 `smelt` 里有提醒类型 `kind` 和 `runtime_id`、`session_id`；正文的提醒类型文案
/// 由 `loc-key` 在 Localizable.strings 里取。格式见 `crates/smelt-push`。
final class PushChannel {
  static let shared = PushChannel()

  private var channel: FlutterMethodChannel?
  /// Dart 取过冷启动目标之后才算接上：之前点开的通知先记着，等它来取。
  private var dartReady = false
  private var launchTarget: [String: String]?
  private var deviceToken: String?
  private var tokenWaiters: [FlutterResult] = []

  func attach(messenger: FlutterBinaryMessenger) {
    let channel = FlutterMethodChannel(name: "smelt/push", binaryMessenger: messenger)
    channel.setMethodCallHandler { [weak self] call, result in
      self?.handle(call, result: result)
    }
    self.channel = channel
  }

  private func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    switch call.method {
    case "configuration":
      result(configuration())
    case "authorization":
      UNUserNotificationCenter.current().getNotificationSettings { settings in
        let status: String
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: status = "authorized"
        case .denied: status = "denied"
        default: status = "notDetermined"
        }
        DispatchQueue.main.async { result(status) }
      }
    case "requestAuthorization":
      UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) {
        granted, _ in
        DispatchQueue.main.async { result(granted) }
      }
    case "deviceToken":
      if let token = deviceToken {
        result(token)
        return
      }
      tokenWaiters.append(result)
      // 令牌每次启动都要重新要：系统可能换过。结果从 AppDelegate 的回调回来。
      if tokenWaiters.count == 1 {
        UIApplication.shared.registerForRemoteNotifications()
      }
    case "takeLaunchTarget":
      dartReady = true
      result(launchTarget)
      launchTarget = nil
    case "delivered":
      // 每条带上系统给的 identifier 和提醒类型：同一会话留着多条，撤哪条由 Dart 按类型挑。
      UNUserNotificationCenter.current().getDeliveredNotifications { notifications in
        let delivered = notifications.compactMap { notification -> [String: String]? in
          guard var target = Self.target(of: notification) else { return nil }
          target["id"] = notification.request.identifier
          target["kind"] = Self.kind(of: notification)
          return target
        }
        DispatchQueue.main.async { result(delivered) }
      }
    case "removeDelivered":
      let ids = call.arguments as? [String] ?? []
      UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
      result(nil)
    case "openSettings":
      let url: URL?
      if #available(iOS 16.0, *) {
        url = URL(string: UIApplication.openNotificationSettingsURLString)
      } else {
        url = URL(string: UIApplication.openSettingsURLString)
      }
      if let url { UIApplication.shared.open(url) }
      result(nil)
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  /// Info.plist 里编译期写好的推送配置。没配中继时为 nil。
  private func configuration() -> [String: String]? {
    let info = Bundle.main.infoDictionary ?? [:]
    let host = (info["SmeltPushRelayHost"] as? String)?.trimmingCharacters(in: .whitespaces) ?? ""
    let environment = info["SmeltApnsEnvironment"] as? String ?? ""
    guard !host.isEmpty, !environment.isEmpty, let topic = Bundle.main.bundleIdentifier else {
      return nil
    }
    return ["relayUrl": "https://\(host)", "environment": environment, "topic": topic]
  }

  func registered(token: Data) {
    let hex = token.map { String(format: "%02x", $0) }.joined()
    deviceToken = hex
    let waiters = tokenWaiters
    tokenWaiters.removeAll()
    waiters.forEach { $0(hex) }
  }

  func failedToRegister(_ error: Error) {
    let waiters = tokenWaiters
    tokenWaiters.removeAll()
    waiters.forEach {
      $0(FlutterError(code: "apns", message: error.localizedDescription, details: nil))
    }
  }

  /// 用户点了一条推送。
  func opened(_ notification: UNNotification) {
    guard let target = Self.target(of: notification) else { return }
    if dartReady, let channel {
      channel.invokeMethod("open", arguments: target)
    } else {
      launchTarget = target
    }
  }

  private static func kind(of notification: UNNotification) -> String? {
    (notification.request.content.userInfo["smelt"] as? [String: Any])?["kind"] as? String
  }

  /// 智能体从命令行、自动化发的消息不带会话（`session_id` 为空），点开只打开 App。
  private static func target(of notification: UNNotification) -> [String: String]? {
    guard let smelt = notification.request.content.userInfo["smelt"] as? [String: Any],
      let runtimeId = smelt["runtime_id"] as? String,
      let sessionId = smelt["session_id"] as? String,
      !sessionId.isEmpty
    else { return nil }
    return ["runtimeId": runtimeId, "sessionId": sessionId]
  }
}
