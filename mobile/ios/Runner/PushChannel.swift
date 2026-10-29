import Flutter
import UIKit
import UserNotifications

/// 推送通知的原生一侧（`lib/data/push.dart` 的 `MethodChannelPushPlatform`）：通知权限、
/// APNs 令牌、通知点击、通知中心里已送达的推送。
///
/// 推送载荷只有提醒类型（`loc-key`，文案在 Localizable.strings）和 `smelt` 里的
/// `runtime_id`、`session_id`，见 `crates/smelt-push`。
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
      UNUserNotificationCenter.current().getDeliveredNotifications { notifications in
        let targets = notifications.compactMap { Self.target(of: $0) }
        DispatchQueue.main.async { result(targets) }
      }
    case "removeDelivered":
      let wanted = Set(
        (call.arguments as? [[String: String]] ?? []).map { "\($0["runtimeId"] ?? "")/\($0["sessionId"] ?? "")" })
      UNUserNotificationCenter.current().getDeliveredNotifications { notifications in
        let ids = notifications.compactMap { notification -> String? in
          guard let target = Self.target(of: notification),
            wanted.contains("\(target["runtimeId"]!)/\(target["sessionId"]!)")
          else { return nil }
          return notification.request.identifier
        }
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: ids)
        DispatchQueue.main.async { result(nil) }
      }
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

  private static func target(of notification: UNNotification) -> [String: String]? {
    guard let smelt = notification.request.content.userInfo["smelt"] as? [String: Any],
      let runtimeId = smelt["runtime_id"] as? String,
      let sessionId = smelt["session_id"] as? String
    else { return nil }
    return ["runtimeId": runtimeId, "sessionId": sessionId]
  }
}
