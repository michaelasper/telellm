#!/usr/bin/env bash
set -euo pipefail

ip6tables_enabled=0
if command -v ip6tables >/dev/null 2>&1 && ip6tables -L OUTPUT >/dev/null 2>&1; then
  ip6tables -P OUTPUT ACCEPT
  ip6tables_enabled=1
fi

allow_dns_to_ipv4() {
  local ip="$1"
  if [[ -n "$ip" && "$ip" != *:* ]]; then
    iptables -A OUTPUT -d "$ip" -p udp --dport 53 -j ACCEPT
    iptables -A OUTPUT -d "$ip" -p tcp --dport 53 -j ACCEPT
  fi
}

allow_all_to_ipv4() {
  local ip="$1"
  if [[ -n "$ip" && "$ip" != *:* ]]; then
    iptables -A OUTPUT -d "$ip" -j ACCEPT
  fi
}

while read -r directive nameserver _; do
  if [[ "$directive" == "nameserver" ]]; then
    allow_dns_to_ipv4 "${nameserver:-}"
  fi
done < /etc/resolv.conf

while IFS= read -r upstream_dns_ip; do
  allow_dns_to_ipv4 "$upstream_dns_ip"
done < <(sed -nE 's/.*ExtServers: \[host\(([0-9.]+)\)\].*/\1/p' /etc/resolv.conf)

docker_dns_ip="${DOCKER_DNS_IP:-127.0.0.11}"
allow_all_to_ipv4 "$docker_dns_ip"
allow_dns_to_ipv4 "$docker_dns_ip"

if [[ -n "${TELELLM_BROKER_HOST:-}" ]]; then
  broker_ip="$(getent hosts "$TELELLM_BROKER_HOST" | awk '{ print $1 }' | head -n 1)"
  if [[ -n "$broker_ip" ]]; then
    case "$broker_ip" in
      *:*)
        if [[ "$ip6tables_enabled" == "1" ]]; then
          ip6tables -A OUTPUT -d "$broker_ip" -p tcp --dport "${TELELLM_BROKER_PORT:-8189}" -j ACCEPT
        fi
        ;;
      *)
        iptables -A OUTPUT -d "$broker_ip" -p tcp --dport "${TELELLM_BROKER_PORT:-8189}" -j ACCEPT
        ;;
    esac
  fi
fi

for cidr in \
  0.0.0.0/8 \
  10.0.0.0/8 \
  169.254.0.0/16 \
  172.16.0.0/12 \
  192.168.0.0/16 \
  224.0.0.0/4
do
  iptables -A OUTPUT -d "$cidr" -j REJECT
done

if [[ "$ip6tables_enabled" == "1" ]]; then
  for cidr in \
    ::/128 \
    ::1/128 \
    fc00::/7 \
    fe80::/10 \
    ff00::/8
  do
    ip6tables -A OUTPUT -d "$cidr" -j REJECT
  done
fi

if [[ -f /run/telellm/codex-auth.json ]]; then
  install -d -m 700 -o codex -g codex /home/codex/.codex
  install -m 600 -o codex -g codex /run/telellm/codex-auth.json /home/codex/.codex/auth.json
fi

codex_config="$(mktemp)"
{
  printf '[projects."/workspace"]\n'
  printf 'trust_level = "trusted"\n'
  printf '\n[notice]\n'
  printf 'hide_full_access_warning = true\n'
} > "$codex_config"
install -m 600 -o codex -g codex "$codex_config" /home/codex/.codex/config.toml
rm -f "$codex_config"

chown codex:codex /workspace

exec runuser --preserve-environment -u codex -- "$@"
