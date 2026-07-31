#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/smelt-relay-deploy.XXXXXX")"
trap 'rm -rf "$FIXTURE"' EXIT
mkdir -p "$FIXTURE/bin"

cat >"$FIXTURE/bin/ssh" <<'EOF'
#!/usr/bin/env bash
printf 'ssh %s\n' "$*" >>"$DEPLOY_TEST_LOG"
if [[ "$*" == *"uname -m"* ]]; then
  echo x86_64
fi
EOF

cat >"$FIXTURE/bin/scp" <<'EOF'
#!/usr/bin/env bash
printf 'scp %s\n' "$*" >>"$DEPLOY_TEST_LOG"
EOF

cat >"$FIXTURE/iroh-relay" <<'EOF'
#!/usr/bin/env bash
echo "iroh-relay 1.0.2"
EOF

chmod +x "$FIXTURE/bin/ssh" "$FIXTURE/bin/scp" "$FIXTURE/iroh-relay"

DEPLOY_TEST_LOG="$FIXTURE/calls.log" PATH="$FIXTURE/bin:$PATH" \
  "$ROOT/scripts/deploy-iroh-relay.sh" \
  --ssh ubuntu@relay.example.com \
  --domain relay.example.com \
  --email admin@example.com \
  --binary "$FIXTURE/iroh-relay"

grep -q '^ssh ubuntu@relay.example.com uname -m$' "$FIXTURE/calls.log"
grep -q 'scp .*smelt-deploy-iroh-relay' "$FIXTURE/calls.log"
grep -q 'ssh -t ubuntu@relay.example.com sudo bash .* --server ' "$FIXTURE/calls.log"
if grep -q 'token' "$FIXTURE/calls.log"; then
  echo "relay token handling should not appear in deployment calls" >&2
  exit 1
fi

echo "relay deployment SSH orchestration test passed"
