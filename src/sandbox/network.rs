#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub blocked_cidrs: Vec<&'static str>,
    pub broker_host: String,
    pub broker_port: u16,
}

impl NetworkPolicy {
    pub fn new(broker_host: impl Into<String>, broker_port: u16) -> Self {
        Self {
            blocked_cidrs: vec![
                "0.0.0.0/8",
                "10.0.0.0/8",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "224.0.0.0/4",
            ],
            broker_host: broker_host.into(),
            broker_port,
        }
    }

    pub fn enforcement_script(&self) -> String {
        let mut script = String::from("#!/usr/bin/env sh\nset -eu\n");
        script.push_str("iptables -P OUTPUT ACCEPT\n");
        script.push_str("broker_host=\"${TELELLM_BROKER_HOST:-");
        script.push_str(&self.broker_host);
        script.push_str("}\"\n");
        script.push_str("broker_port=\"${TELELLM_BROKER_PORT:-");
        script.push_str(&self.broker_port.to_string());
        script.push_str("}\"\n");
        script.push_str("while read -r directive nameserver _; do\n");
        script.push_str("  case \"$nameserver\" in *:*) continue ;; esac\n");
        script
            .push_str("  if [ \"$directive\" = \"nameserver\" ] && [ -n \"$nameserver\" ]; then\n");
        script.push_str("    iptables -A OUTPUT -d \"$nameserver\" -p udp --dport 53 -j ACCEPT\n");
        script.push_str("    iptables -A OUTPUT -d \"$nameserver\" -p tcp --dport 53 -j ACCEPT\n");
        script.push_str("  fi\n");
        script.push_str("done < /etc/resolv.conf\n");
        script.push_str("docker_dns_ip=\"${DOCKER_DNS_IP:-127.0.0.11}\"\n");
        script.push_str("iptables -A OUTPUT -d \"$docker_dns_ip\" -p udp --dport 53 -j ACCEPT\n");
        script.push_str("iptables -A OUTPUT -d \"$docker_dns_ip\" -p tcp --dport 53 -j ACCEPT\n");
        script.push_str("if [ -n \"$broker_host\" ]; then\n");
        script.push_str(
            "  broker_ip=$(getent hosts \"$broker_host\" | awk '{ print $1 }' | head -n 1)\n",
        );
        script.push_str("  if [ -n \"$broker_ip\" ]; then\n");
        script.push_str(
            "    iptables -A OUTPUT -d \"$broker_ip\" -p tcp --dport \"$broker_port\" -j ACCEPT\n",
        );
        script.push_str("  fi\n");
        script.push_str("fi\n");
        for cidr in &self.blocked_cidrs {
            script.push_str("iptables -A OUTPUT -d ");
            script.push_str(cidr);
            script.push_str(" -j REJECT\n");
        }
        script.push_str("exec \"$@\"\n");
        script
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforcement_script_should_reject_private_lan_ranges() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();

        assert!(script.contains("iptables -A OUTPUT -d 192.168.0.0/16 -j REJECT"));
    }

    #[test]
    fn enforcement_script_should_allow_configured_broker_before_reject_rules() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();
        let broker_rule = script.find("-p tcp --dport \"$broker_port\" -j ACCEPT");
        let private_lan_rule = script.find("iptables -A OUTPUT -d 192.168.0.0/16 -j REJECT");

        assert!(matches!(
            (broker_rule, private_lan_rule),
            (Some(broker), Some(private_lan)) if broker < private_lan
        ));
    }

    #[test]
    fn enforcement_script_should_allow_docker_dns_before_loopback_reject() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();
        let dns_rule = script.find("-d \"$docker_dns_ip\" -p udp --dport 53 -j ACCEPT");
        let loopback_rule = script.find("iptables -A OUTPUT -d 127.0.0.0/8 -j REJECT");

        assert!(matches!(
            (dns_rule, loopback_rule),
            (Some(dns), Some(loopback)) if dns < loopback
        ));
    }

    #[test]
    fn enforcement_script_should_allow_resolv_conf_dns_before_private_lan_reject() {
        let policy = NetworkPolicy::new("host.docker.internal", 8189);

        let script = policy.enforcement_script();
        let dns_rule = script.find("-d \"$nameserver\" -p udp --dport 53 -j ACCEPT");
        let private_lan_rule = script.find("iptables -A OUTPUT -d 192.168.0.0/16 -j REJECT");

        assert!(matches!(
            (dns_rule, private_lan_rule),
            (Some(dns), Some(private_lan)) if dns < private_lan
        ));
    }
}
