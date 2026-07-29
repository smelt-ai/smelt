import 'dart:convert';
import 'dart:io';

class PairingConfig {
  const PairingConfig({required this.endpoint, required this.token});

  final String endpoint;
  final String token;

  factory PairingConfig.fromFields(String endpoint, String token) {
    final trimmedEndpoint = endpoint.trim();
    final trimmedToken = token.trim();
    if (trimmedEndpoint.isEmpty || trimmedToken.isEmpty) {
      throw const FormatException('Endpoint and token are required');
    }
    return _fromUri(Uri.parse(trimmedEndpoint), token: trimmedToken);
  }

  factory PairingConfig.parse(String value) {
    final raw = value.trim();
    if (raw.isEmpty) throw const FormatException('The QR code is empty');

    if (raw.startsWith('{')) {
      final decoded = jsonDecode(raw);
      if (decoded is! Map<String, dynamic>) {
        throw const FormatException('Invalid Smelt pairing code');
      }
      final endpoint = decoded['endpoint'] as String?;
      final token = decoded['token'] as String?;
      if (endpoint == null || token == null) {
        throw const FormatException(
          'Pairing code is missing endpoint or token',
        );
      }
      return PairingConfig.fromFields(endpoint, token);
    }

    return _fromUri(Uri.parse(raw));
  }

  static PairingConfig _fromUri(Uri uri, {String? token}) {
    if (!const {'http', 'https', 'ws', 'wss'}.contains(uri.scheme) ||
        uri.host.isEmpty) {
      throw const FormatException('Unsupported Smelt gateway address');
    }
    // The token travels in the query string and the whole ACP session is
    // unauthenticated at the transport layer, so cleartext is only tolerated
    // for gateways reachable on the local network. This is the single
    // enforcement point for both platforms: Android cannot express private IP
    // ranges in its network security config, and iOS only relaxes ATS for
    // local networking.
    if (const {'http', 'ws'}.contains(uri.scheme) && !_isLocalHost(uri.host)) {
      throw FormatException(
        'Refusing to send the pairing token in cleartext to ${uri.host}. '
        'Use an https:// or wss:// address for gateways outside your local '
        'network.',
      );
    }
    if (uri.queryParameters.containsKey('room') ||
        uri.queryParameters.containsKey('signal')) {
      throw const FormatException(
        'This WebRTC sharing code is not supported by the mobile app yet',
      );
    }

    final resolvedToken = (token ?? uri.queryParameters['token'] ?? '').trim();
    if (resolvedToken.isEmpty) {
      throw const FormatException('Pairing code is missing its token');
    }

    final remainingQuery = Map<String, String>.of(uri.queryParameters)
      ..remove('token');
    final endpoint = Uri(
      scheme: uri.scheme,
      userInfo: uri.userInfo,
      host: uri.host,
      port: uri.hasPort ? uri.port : null,
      path: uri.path,
      queryParameters: remainingQuery.isEmpty ? null : remainingQuery,
    );
    return PairingConfig(endpoint: endpoint.toString(), token: resolvedToken);
  }

  /// Whether [host] is loopback, link-local, or an RFC1918/ULA private address
  /// — i.e. somewhere cleartext traffic never leaves the user's own network.
  static bool _isLocalHost(String host) {
    final normalized = host.toLowerCase();
    if (normalized == 'localhost' ||
        normalized.endsWith('.localhost') ||
        normalized == 'local' ||
        normalized.endsWith('.local')) {
      return true;
    }

    final address = InternetAddress.tryParse(normalized);
    if (address == null) return false;
    final bytes = address.rawAddress;
    if (address.type == InternetAddressType.IPv4) {
      return _isPrivateIPv4(bytes);
    }
    if (bytes.length != 16) return false;
    // ::1
    if (bytes.take(15).every((byte) => byte == 0) && bytes[15] == 1) {
      return true;
    }
    // ::ffff:a.b.c.d
    final isIPv4Mapped =
        bytes.take(10).every((byte) => byte == 0) &&
        bytes[10] == 0xff &&
        bytes[11] == 0xff;
    if (isIPv4Mapped) return _isPrivateIPv4(bytes.sublist(12));
    if ((bytes[0] & 0xfe) == 0xfc) return true; // fc00::/7 unique local
    if (bytes[0] == 0xfe && (bytes[1] & 0xc0) == 0x80) return true; // fe80::/10
    return false;
  }

  static bool _isPrivateIPv4(List<int> bytes) {
    if (bytes.length != 4) return false;
    if (bytes[0] == 127) return true; // 127.0.0.0/8
    if (bytes[0] == 10) return true; // 10.0.0.0/8
    if (bytes[0] == 172 && bytes[1] >= 16 && bytes[1] <= 31) {
      return true; // 172.16.0.0/12
    }
    if (bytes[0] == 192 && bytes[1] == 168) return true; // 192.168.0.0/16
    if (bytes[0] == 169 && bytes[1] == 254) return true; // 169.254.0.0/16
    return false;
  }
}
