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
printf '%s\n' '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef' \
  >"$FIXTURE/token"

DEPLOY_TEST_LOG="$FIXTURE/calls.log" PATH="$FIXTURE/bin:$PATH" \
  "$ROOT/scripts/deploy-iroh-relay.sh" \
  --ssh ubuntu@relay.example.com \
  --domain relay.example.com \
  --email admin@example.com \
  --token-file "$FIXTURE/token" \
  --binary "$FIXTURE/iroh-relay"

grep -q '^ssh ubuntu@relay.example.com uname -m$' "$FIXTURE/calls.log"
grep -q 'scp .*smelt-deploy-iroh-relay' "$FIXTURE/calls.log"
grep -q 'scp .*smelt-iroh-relay-token' "$FIXTURE/calls.log"
grep -q 'ssh -t ubuntu@relay.example.com sudo bash .* --server ' "$FIXTURE/calls.log"
if grep -q '0123456789abcdef' "$FIXTURE/calls.log"; then
  echo "token leaked into SSH/SCP command line" >&2
  exit 1
fi

printf '%s\n' short >"$FIXTURE/bad-token"
set +e
OUTPUT="$(
  DEPLOY_TEST_LOG="$FIXTURE/bad-calls.log" PATH="$FIXTURE/bin:$PATH" \
    "$ROOT/scripts/deploy-iroh-relay.sh" \
    --ssh ubuntu@relay.example.com \
    --domain relay.example.com \
    --email admin@example.com \
    --token-file "$FIXTURE/bad-token" \
    --binary "$FIXTURE/iroh-relay" 2>&1
)"
STATUS=$?
set -e
if [[ "$STATUS" -eq 0 || "$OUTPUT" != *"Relay token must be 32-256 characters"* ]]; then
  echo "invalid token was not rejected" >&2
  echo "$OUTPUT" >&2
  exit 1
fi

echo "relay deployment SSH orchestration test passed"
