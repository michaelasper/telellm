#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TELELLM_BROKER_HOST:-}" ]]; then
  broker_ip="$(getent hosts "$TELELLM_BROKER_HOST" | awk '{ print $1 }' | head -n 1)"
  if [[ -n "$broker_ip" ]]; then
    iptables -A OUTPUT -d "$broker_ip" -p tcp --dport "${TELELLM_BROKER_PORT:-8189}" -j ACCEPT
  fi
fi

for cidr in \
  0.0.0.0/8 \
  10.0.0.0/8 \
  127.0.0.0/8 \
  169.254.0.0/16 \
  172.16.0.0/12 \
  192.168.0.0/16 \
  224.0.0.0/4
do
  iptables -A OUTPUT -d "$cidr" -j REJECT
done

chown codex:codex /workspace

exec runuser --preserve-environment -u codex -- "$@"
