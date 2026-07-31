#!/usr/bin/env bash
# Deploy the iroh relay used by Smelt to a dedicated Debian/Ubuntu server.
# Run locally with --ssh, or copy this script to the server and run it as root.
set -euo pipefail

IROH_VERSION="1.0.2"
DOMAIN=""
EMAIL=""
SSH_TARGET=""
BINARY_PATH=""
TOKEN_FILE=""
PROMPT_TOKEN=false
SERVER_MODE=false
DOWNLOAD_PREFIX=""
EXPECTED_SHA256=""

usage() {
    cat <<'EOF'
Usage:
  deploy-iroh-relay.sh --ssh USER@HOST --domain DOMAIN --email EMAIL --prompt-token
  sudo deploy-iroh-relay.sh --domain DOMAIN --email EMAIL --prompt-token

Options:
  --ssh USER@HOST        Deploy from this machine over SSH. The verified binary is
                         downloaded locally and uploaded to the server.
  --domain DOMAIN        Public DNS name for the relay, for example relay.example.com.
  --email EMAIL          Let's Encrypt contact email.
  --prompt-token         Read the Relay access token without echo. Generate it in
                         Smelt Settings > Remote, then paste it here.
  --token-file PATH      Read the token from a 0600 file instead of prompting.
  --binary PATH          Use this iroh-relay binary instead of downloading a release.
  --version VERSION      iroh-relay version (default: 1.0.2).
  --sha256 HEX           Required for a downloaded version not built into this script.
  --download-prefix URL  Prefix the official GitHub URL with a download proxy URL.
  -h, --help             Show this help.

The script only manages:
  /usr/local/bin/iroh-relay
  /etc/iroh-relay
  /var/lib/iroh-relay
  /etc/systemd/system/iroh-relay.service

It does not change cloud security groups, UFW, Nginx, WireGuard, or other services.
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

log() {
    echo "==> $*"
}

while (($#)); do
    case "$1" in
        --ssh)
            (($# >= 2)) || die "--ssh requires a value"
            SSH_TARGET="$2"
            shift 2
            ;;
        --domain)
            (($# >= 2)) || die "--domain requires a value"
            DOMAIN="$2"
            shift 2
            ;;
        --email)
            (($# >= 2)) || die "--email requires a value"
            EMAIL="$2"
            shift 2
            ;;
        --prompt-token)
            PROMPT_TOKEN=true
            shift
            ;;
        --token-file)
            (($# >= 2)) || die "--token-file requires a value"
            TOKEN_FILE="$2"
            shift 2
            ;;
        --binary)
            (($# >= 2)) || die "--binary requires a value"
            BINARY_PATH="$2"
            shift 2
            ;;
        --version)
            (($# >= 2)) || die "--version requires a value"
            IROH_VERSION="$2"
            shift 2
            ;;
        --sha256)
            (($# >= 2)) || die "--sha256 requires a value"
            EXPECTED_SHA256="$2"
            shift 2
            ;;
        --download-prefix)
            (($# >= 2)) || die "--download-prefix requires a value"
            DOWNLOAD_PREFIX="$2"
            shift 2
            ;;
        --server)
            SERVER_MODE=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

[[ "$DOMAIN" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]] \
    || die "invalid --domain: $DOMAIN"
[[ "$DOMAIN" == *.* ]] || die "--domain must be a public DNS name, not a bare host name"
[[ "$EMAIL" =~ ^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,63}$ ]] \
    || die "invalid --email: $EMAIL"
[[ "$IROH_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "invalid --version: $IROH_VERSION"
[[ -z "$SSH_TARGET" || "$SSH_TARGET" =~ ^[A-Za-z0-9_.@:-]+$ ]] \
    || die "invalid --ssh target"
[[ -z "$DOWNLOAD_PREFIX" || "$DOWNLOAD_PREFIX" =~ ^https?:// ]] \
    || die "--download-prefix must be an HTTP(S) URL"
[[ -z "$EXPECTED_SHA256" || "$EXPECTED_SHA256" =~ ^[0-9a-fA-F]{64}$ ]] \
    || die "--sha256 must contain 64 hexadecimal characters"
[[ "$PROMPT_TOKEN" == false || -z "$TOKEN_FILE" ]] \
    || die "use only one of --prompt-token and --token-file"

normalize_arch() {
    case "$1" in
        x86_64|amd64) echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) die "unsupported architecture: $1 (expected x86_64 or aarch64)" ;;
    esac
}

release_checksum() {
    local arch="$1"
    if [[ -n "$EXPECTED_SHA256" ]]; then
        printf '%s' "$EXPECTED_SHA256" | tr '[:upper:]' '[:lower:]'
        return
    fi
    case "$IROH_VERSION:$arch" in
        1.0.2:x86_64) echo "3d6c37a66f8b21da620f9d83ce4682639aa2de9910bbf1e8e7981cf8478964ea" ;;
        1.0.2:aarch64) echo "9a548087f7b1f3a25f5c932790bc0836dd3cb6ffb28d6104b63d18478ed2c51d" ;;
        *) die "no built-in checksum for iroh-relay $IROH_VERSION ($arch); pass --sha256" ;;
    esac
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | awk '{print $1}'
    else
        die "sha256sum or shasum is required"
    fi
}

download_binary() {
    local arch="$1"
    local output="$2"
    local temp_dir archive asset official_url url expected actual
    command -v curl >/dev/null 2>&1 || die "curl is required to download iroh-relay"
    command -v tar >/dev/null 2>&1 || die "tar is required to unpack iroh-relay"
    temp_dir="$(mktemp -d)"
    archive="$temp_dir/iroh-relay.tar.gz"
    asset="iroh-relay-v${IROH_VERSION}-${arch}-unknown-linux-musl.tar.gz"
    official_url="https://github.com/n0-computer/iroh/releases/download/v${IROH_VERSION}/${asset}"
    url="${DOWNLOAD_PREFIX}${official_url}"
    expected="$(release_checksum "$arch")"

    log "downloading $asset"
    if ! curl --fail --location --retry 3 --connect-timeout 15 --output "$archive" "$url"; then
        rm -rf "$temp_dir"
        die "failed to download $asset"
    fi
    actual="$(sha256_file "$archive")"
    if [[ "$actual" != "$expected" ]]; then
        rm -rf "$temp_dir"
        die "checksum mismatch for $asset (expected $expected, got $actual)"
    fi
    if ! tar -xzf "$archive" -C "$temp_dir"; then
        rm -rf "$temp_dir"
        die "failed to unpack $asset"
    fi
    if [[ ! -x "$temp_dir/iroh-relay" ]]; then
        rm -rf "$temp_dir"
        die "release archive did not contain iroh-relay"
    fi
    install -m 0755 "$temp_dir/iroh-relay" "$output"
    rm -rf "$temp_dir"
}

validate_token() {
    ((${#1} >= 32 && ${#1} <= 256)) && [[ "$1" =~ ^[A-Za-z0-9._~-]+$ ]] \
        || die "Relay token must be 32-256 characters using A-Z, a-z, 0-9, '.', '_', '~', or '-'"
}

deploy_over_ssh() {
    command -v ssh >/dev/null 2>&1 || die "ssh is required"
    command -v scp >/dev/null 2>&1 || die "scp is required"
    [[ -f "$0" ]] || die "cannot locate this deployment script"

    local temp_dir remote_script remote_binary remote_token local_binary local_token token
    local remote_arch remote_command arg
    local -a remote_args remote_cleanup
    temp_dir="$(mktemp -d)"
    remote_script="/tmp/smelt-deploy-iroh-relay-$$.sh"
    remote_binary="/tmp/smelt-iroh-relay-$$"
    remote_token="/tmp/smelt-iroh-relay-token-$$"
    remote_cleanup=("$remote_script")

    cleanup_remote_deploy() {
        rm -rf "$temp_dir"
        if ((${#remote_cleanup[@]})); then
            local cleanup_command="rm -f"
            for arg in "${remote_cleanup[@]}"; do
                printf -v cleanup_command '%s %q' "$cleanup_command" "$arg"
            done
            ssh "$SSH_TARGET" "$cleanup_command" >/dev/null 2>&1 || true
        fi
    }
    trap cleanup_remote_deploy EXIT

    log "checking remote architecture"
    remote_arch="$(normalize_arch "$(ssh "$SSH_TARGET" uname -m)")"
    if [[ -n "$BINARY_PATH" ]]; then
        [[ -x "$BINARY_PATH" ]] || die "--binary is not executable: $BINARY_PATH"
        local_binary="$BINARY_PATH"
    else
        local_binary="$temp_dir/iroh-relay"
        download_binary "$remote_arch" "$local_binary"
    fi

    if [[ "$PROMPT_TOKEN" == true ]]; then
        read -r -s -p "Paste the Relay token generated by Smelt: " token
        echo
        validate_token "$token"
        local_token="$temp_dir/relay-token"
        umask 077
        printf '%s\n' "$token" >"$local_token"
    elif [[ -n "$TOKEN_FILE" ]]; then
        [[ -f "$TOKEN_FILE" ]] || die "token file not found: $TOKEN_FILE"
        token="$(tr -d '\r\n' <"$TOKEN_FILE")"
        validate_token "$token"
        local_token="$TOKEN_FILE"
    else
        local_token=""
    fi

    log "uploading deployment files to $SSH_TARGET"
    scp -q "$0" "$SSH_TARGET:$remote_script"
    scp -q "$local_binary" "$SSH_TARGET:$remote_binary"
    remote_cleanup+=("$remote_binary")

    remote_args=(sudo bash "$remote_script" --server --domain "$DOMAIN" --email "$EMAIL"
        --version "$IROH_VERSION" --binary "$remote_binary")
    if [[ -n "$local_token" ]]; then
        scp -q -p "$local_token" "$SSH_TARGET:$remote_token"
        remote_cleanup+=("$remote_token")
        remote_args+=(--token-file "$remote_token")
    fi

    printf -v remote_command '%q ' "${remote_args[@]}"
    log "installing iroh-relay on $SSH_TARGET"
    ssh -t "$SSH_TARGET" "$remote_command"
    cleanup_remote_deploy
    trap - EXIT
}

if [[ -n "$SSH_TARGET" && "$SERVER_MODE" == false ]]; then
    deploy_over_ssh
    exit 0
fi

[[ -z "$SSH_TARGET" ]] || die "--ssh cannot be combined with internal --server mode"
((EUID == 0)) || die "run as root (or use --ssh so the script can invoke sudo remotely)"
[[ "$(uname -s)" == "Linux" ]] || die "server-side installation only supports Linux"

install_dependencies() {
    local -a packages=()
    command -v openssl >/dev/null 2>&1 || packages+=(openssl)
    command -v ss >/dev/null 2>&1 || packages+=(iproute2)
    command -v curl >/dev/null 2>&1 || packages+=(curl ca-certificates)
    command -v getent >/dev/null 2>&1 || packages+=(libc-bin)
    command -v useradd >/dev/null 2>&1 || packages+=(passwd)
    command -v install >/dev/null 2>&1 || packages+=(coreutils)
    if [[ -z "$BINARY_PATH" ]]; then
        command -v tar >/dev/null 2>&1 || packages+=(tar)
    fi
    if ((${#packages[@]})); then
        command -v apt-get >/dev/null 2>&1 \
            || die "missing ${packages[*]}; automatic dependency installation requires Debian/Ubuntu"
        log "installing dependencies: ${packages[*]}"
        apt-get update
        env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${packages[@]}"
    fi
}

port_owner() {
    local protocol="$1" port="$2"
    if [[ "$protocol" == "tcp" ]]; then
        ss -H -ltnp 2>/dev/null | awk -v suffix=":$port" '$4 ~ suffix "$" {print}'
    else
        ss -H -lunp 2>/dev/null | awk -v suffix=":$port" '$4 ~ suffix "$" {print}'
    fi
}

ensure_port_available() {
    local protocol="$1" port="$2" owner
    owner="$(port_owner "$protocol" "$port")"
    if [[ -n "$owner" && "$owner" != *'"iroh-relay"'* ]]; then
        echo "$owner" >&2
        die "$protocol port $port is already used by another service; this script will not replace it"
    fi
}

install_dependencies
ensure_port_available tcp 80
ensure_port_available tcp 443
ensure_port_available udp 7842

if ! getent hosts "$DOMAIN" >/dev/null 2>&1; then
    die "$DOMAIN does not resolve yet; create its public DNS A/AAAA record before deploying"
fi

if [[ "$PROMPT_TOKEN" == true ]]; then
    read -r -s -p "Paste the Relay token generated by Smelt: " RELAY_TOKEN
    echo
elif [[ -n "$TOKEN_FILE" ]]; then
    [[ -f "$TOKEN_FILE" ]] || die "token file not found: $TOKEN_FILE"
    RELAY_TOKEN="$(tr -d '\r\n' <"$TOKEN_FILE")"
elif [[ -f /etc/iroh-relay/relay.env ]]; then
    RELAY_TOKEN="$(sed -n 's/^IROH_RELAY_ACCESS_TOKEN=//p' /etc/iroh-relay/relay.env | head -n 1)"
else
    die "first install requires --prompt-token or --token-file; generate the token in Smelt first"
fi
validate_token "$RELAY_TOKEN"

server_arch="$(normalize_arch "$(uname -m)")"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT
if [[ -n "$BINARY_PATH" ]]; then
    [[ -x "$BINARY_PATH" ]] || die "--binary is not executable: $BINARY_PATH"
    binary_source="$BINARY_PATH"
else
    binary_source="$temp_dir/iroh-relay"
    download_binary "$server_arch" "$binary_source"
fi
"$binary_source" --version | grep -F "iroh-relay $IROH_VERSION" >/dev/null \
    || die "binary version does not match --version $IROH_VERSION"

log "creating service account and directories"
if ! id iroh-relay >/dev/null 2>&1; then
    useradd --system --home-dir /var/lib/iroh-relay --shell /usr/sbin/nologin iroh-relay
fi
install -d -o root -g iroh-relay -m 0750 /etc/iroh-relay
install -d -o iroh-relay -g iroh-relay -m 0750 /var/lib/iroh-relay/certs
install -m 0755 "$binary_source" "$temp_dir/iroh-relay.new"
mv -f "$temp_dir/iroh-relay.new" /usr/local/bin/iroh-relay

cat >"$temp_dir/iroh-relay.toml" <<EOF
enable_relay = true
http_bind_addr = "0.0.0.0:80"
enable_quic_addr_discovery = true
enable_metrics = true
metrics_bind_addr = "127.0.0.1:9090"

# The real value is supplied by /etc/iroh-relay/relay.env.
access.shared_token = ["configured-via-environment"]

[tls]
https_bind_addr = "0.0.0.0:443"
quic_bind_addr = "0.0.0.0:7842"
hostname = ["$DOMAIN"]
cert_mode = "LetsEncrypt"
prod_tls = true
contact = "$EMAIL"
cert_dir = "/var/lib/iroh-relay/certs"
EOF
install -o root -g iroh-relay -m 0640 "$temp_dir/iroh-relay.toml" /etc/iroh-relay/iroh-relay.toml

umask 077
printf 'IROH_RELAY_ACCESS_TOKEN=%s\n' "$RELAY_TOKEN" >"$temp_dir/relay.env"
install -o root -g iroh-relay -m 0640 "$temp_dir/relay.env" /etc/iroh-relay/relay.env

cat >"$temp_dir/iroh-relay.service" <<'EOF'
[Unit]
Description=Smelt self-hosted iroh relay
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=iroh-relay
Group=iroh-relay
WorkingDirectory=/var/lib/iroh-relay
EnvironmentFile=/etc/iroh-relay/relay.env
Environment=RUST_LOG=info
ExecStart=/usr/local/bin/iroh-relay --config-path /etc/iroh-relay/iroh-relay.toml
Restart=on-failure
RestartSec=3
StateDirectory=iroh-relay
StateDirectoryMode=0750
UMask=0027
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
RestrictSUIDSGID=true
RestrictRealtime=true
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
EOF
install -o root -g root -m 0644 "$temp_dir/iroh-relay.service" /etc/systemd/system/iroh-relay.service

log "starting iroh-relay"
systemctl daemon-reload
systemctl enable iroh-relay.service >/dev/null
systemctl restart iroh-relay.service
sleep 3
if ! systemctl is-active --quiet iroh-relay.service; then
    journalctl -u iroh-relay.service -n 80 --no-pager >&2
    die "iroh-relay failed to start"
fi

tls_ready=false
for _ in {1..20}; do
    if curl --silent --show-error --connect-timeout 5 --max-time 8 \
        --output /dev/null "https://$DOMAIN/"; then
        tls_ready=true
        break
    fi
    sleep 1
done

echo
echo "iroh-relay $IROH_VERSION is active."
echo "Relay URL: https://$DOMAIN"
echo "Open in the cloud security group: TCP 80, TCP 443, UDP 7842."
echo "Metrics stay local at http://127.0.0.1:9090/metrics."
if [[ "$tls_ready" == true ]]; then
    echo "TLS check: OK"
else
    echo "TLS check: pending; inspect with: journalctl -u iroh-relay -f" >&2
fi
echo "In Smelt Settings > Remote, use $DOMAIN and the same Relay token."
