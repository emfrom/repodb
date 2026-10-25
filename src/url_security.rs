use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs},
    sync::{
        OnceLock,
        mpsc::{Receiver, SyncSender, TrySendError, channel, sync_channel},
    },
    time::{Duration, Instant},
};

type DnsRequest = (
    String,
    Instant,
    std::sync::mpsc::Sender<std::result::Result<Vec<IpAddr>, String>>,
);

// Isolate potentially blocking OS resolution in one worker with a one-request queue.
static DNS_WORKER: OnceLock<std::result::Result<SyncSender<DnsRequest>, String>> = OnceLock::new();
pub(crate) const URL_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Endpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

pub(crate) fn redact_userinfo(input: &str) -> String {
    let Some((scheme, remainder)) = input.split_once("://") else {
        return input.to_owned();
    };
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    let Some((_, host)) = authority.rsplit_once('@') else {
        return input.to_owned();
    };
    format!(
        "{scheme}://[redacted]@{}{suffix}",
        host,
        suffix = &remainder[authority_end..]
    )
}

pub(crate) fn parse_http_url(url: &str) -> crate::Result<Endpoint> {
    let (scheme, remainder) = url.split_once("://").ok_or("URL must use HTTP or HTTPS")?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err("URL must use HTTP or HTTPS".into());
    }
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())
        .ok_or("URL has no host")?;
    if authority.contains('@') {
        return Err("URL credentials are not allowed".into());
    }

    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed.find(']').ok_or("invalid bracketed IPv6 host")?;
        let host = &bracketed[..close];
        host.parse::<Ipv6Addr>()
            .map_err(|_| "invalid IPv6 URL host")?;
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            default_port(&scheme)
        } else {
            suffix
                .strip_prefix(':')
                .ok_or("invalid text after IPv6 URL host")?
                .parse::<u16>()?
        };
        (host.to_ascii_lowercase(), port)
    } else {
        if authority.matches(':').count() > 1 {
            return Err("IPv6 URL hosts must be bracketed".into());
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>()?),
            None => (authority, default_port(&scheme)),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty()
            || !host.is_ascii()
            || host
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
        {
            return Err("URL host must be an ASCII DNS name or IP address".into());
        }
        if host.parse::<Ipv4Addr>().is_err()
            && (host
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
                || host.starts_with("0x"))
        {
            return Err("non-canonical numeric URL hosts are not allowed".into());
        }
        (host, port)
    };
    if port == 0 {
        return Err("URL port must be greater than zero".into());
    }
    Ok(Endpoint { scheme, host, port })
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

pub(crate) fn private_address_allowlist_from_env() -> crate::Result<HashSet<IpAddr>> {
    let value = match std::env::var("REPODB_URL_PRIVATE_ALLOWLIST") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(HashSet::new()),
        Err(error) => return Err(error.into()),
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<IpAddr>()
                .map_err(|_| "REPODB_URL_PRIVATE_ALLOWLIST accepts only IP literals".into())
        })
        .collect()
}

pub(crate) fn resolve_and_validate(
    endpoint: &Endpoint,
    private_allowlist: &HashSet<IpAddr>,
    deadline: Instant,
) -> crate::Result<Vec<IpAddr>> {
    resolve_and_validate_with(endpoint, private_allowlist, |endpoint| {
        resolve_host_by_deadline(endpoint, deadline)
    })
}

fn resolve_and_validate_with(
    endpoint: &Endpoint,
    private_allowlist: &HashSet<IpAddr>,
    resolver: impl FnOnce(&Endpoint) -> crate::Result<Vec<IpAddr>>,
) -> crate::Result<Vec<IpAddr>> {
    let addresses = resolver(endpoint)?;
    validate_addresses(endpoint, &addresses, private_allowlist)
}

fn resolve_host_by_deadline(endpoint: &Endpoint, deadline: Instant) -> crate::Result<Vec<IpAddr>> {
    if let Ok(address) = endpoint.host.parse::<IpAddr>() {
        return Ok(vec![address]);
    }
    let lookup_host = if endpoint.host.contains(':') {
        format!("[{}]:{}", endpoint.host, endpoint.port)
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    };
    let sender = DNS_WORKER
        .get_or_init(|| {
            let (sender, receiver) = sync_channel::<DnsRequest>(1);
            std::thread::Builder::new()
                .name("repodb-dns-resolver".to_owned())
                .spawn(move || {
                    while let Ok((lookup_host, deadline, reply)) = receiver.recv() {
                        let addresses = if Instant::now() >= deadline {
                            Err("DNS resolution exceeded the URL fetch deadline".to_owned())
                        } else {
                            lookup_host
                                .to_socket_addrs()
                                .map(|addresses| {
                                    addresses
                                        .map(|address| address.ip())
                                        .collect::<HashSet<_>>()
                                        .into_iter()
                                        .collect()
                                })
                                .map_err(|error| error.to_string())
                        };
                        let _ = reply.send(addresses);
                    }
                })
                .map(|_| sender)
                .map_err(|error| format!("could not start DNS resolver: {error}"))
        })
        .as_ref()
        .map_err(|error| error.clone())?;
    let (reply, result) = channel();
    match sender.try_send((lookup_host, deadline, reply)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            return Err("DNS resolver is busy after a previous lookup".into());
        }
        Err(TrySendError::Disconnected(_)) => {
            return Err("DNS resolver worker stopped unexpectedly".into());
        }
    }
    receive_dns_result(result, deadline)
}

fn receive_dns_result(
    receiver: Receiver<std::result::Result<Vec<IpAddr>, String>>,
    deadline: Instant,
) -> crate::Result<Vec<IpAddr>> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    let addresses = receiver
        .recv_timeout(timeout)
        .map_err(|error| match error {
            std::sync::mpsc::RecvTimeoutError::Timeout => {
                "DNS resolution exceeded the URL fetch deadline"
            }
            std::sync::mpsc::RecvTimeoutError::Disconnected => "DNS resolver returned no result",
        })?;
    addresses.map_err(Into::into)
}

fn validate_addresses(
    endpoint: &Endpoint,
    addresses: &[IpAddr],
    private_allowlist: &HashSet<IpAddr>,
) -> crate::Result<Vec<IpAddr>> {
    if addresses.is_empty() {
        return Err(format!("URL host {} resolved to no addresses", endpoint.host).into());
    }
    if addresses
        .iter()
        .any(|address| !is_public_address(*address) && !private_allowlist.contains(address))
    {
        return Err(format!(
            "URL host {} resolved to a restricted address; set REPODB_URL_PRIVATE_ALLOWLIST only for explicitly trusted IP literals",
            endpoint.host
        )
        .into());
    }
    Ok(addresses.to_vec())
}

pub(crate) fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn ipv4_in_prefix(address: Ipv4Addr, network: Ipv4Addr, prefix: u32) -> bool {
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    u32::from(address) & mask == u32::from(network) & mask
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    ![
        (Ipv4Addr::new(0, 0, 0, 0), 8),
        (Ipv4Addr::new(10, 0, 0, 0), 8),
        (Ipv4Addr::new(100, 64, 0, 0), 10),
        (Ipv4Addr::new(127, 0, 0, 0), 8),
        (Ipv4Addr::new(169, 254, 0, 0), 16),
        (Ipv4Addr::new(172, 16, 0, 0), 12),
        (Ipv4Addr::new(192, 0, 0, 0), 24),
        (Ipv4Addr::new(192, 0, 2, 0), 24),
        (Ipv4Addr::new(192, 88, 99, 0), 24),
        (Ipv4Addr::new(192, 168, 0, 0), 16),
        (Ipv4Addr::new(198, 18, 0, 0), 15),
        (Ipv4Addr::new(198, 51, 100, 0), 24),
        (Ipv4Addr::new(203, 0, 113, 0), 24),
        (Ipv4Addr::new(224, 0, 0, 0), 4),
        (Ipv4Addr::new(240, 0, 0, 0), 4),
    ]
    .iter()
    .any(|(network, prefix)| ipv4_in_prefix(address, *network, *prefix))
        && !address.is_broadcast()
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    // Only global-unicast 2000::/3 addresses are eligible. Exclude documented,
    // transition and special-purpose subranges that are not ordinary public hosts.
    (segments[0] & 0xe000) == 0x2000
        && !(segments[0] == 0x2000 && segments[1] <= 0x01ff)
        && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
        && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        && segments[0] != 0x2002
        && !(segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
}

pub(crate) fn resolve_entries(endpoint: &Endpoint, addresses: &[IpAddr]) -> Vec<String> {
    if endpoint.host.parse::<IpAddr>().is_ok() {
        return Vec::new();
    }
    addresses
        .iter()
        .map(|address| {
            let address = match address {
                IpAddr::V4(address) => address.to_string(),
                IpAddr::V6(address) => format!("[{address}]"),
            };
            format!("{}:{}:{address}", endpoint.host, endpoint.port)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_address_filter_rejects_restricted_ipv4_and_ipv6_ranges() {
        for address in [
            "0.1.2.3",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
        ] {
            let address = address.parse().unwrap();
            assert!(!is_public_address(address), "{address} must be restricted");
        }
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "2001:4860::8888",
        ] {
            let address = address.parse().unwrap();
            assert!(is_public_address(address), "{address} must be public");
        }
    }

    #[test]
    fn userinfo_redaction_preserves_the_url_without_exposing_credentials() {
        assert_eq!(
            redact_userinfo("https://alice:secret@example.com/path?q=1"),
            "https://[redacted]@example.com/path?q=1"
        );
        assert_eq!(
            redact_userinfo("ssh://git@example.com/repo"),
            "ssh://[redacted]@example.com/repo"
        );
        assert_eq!(
            redact_userinfo("https://example.com/path"),
            "https://example.com/path"
        );
        assert_eq!(redact_userinfo("local/path"), "local/path");
    }

    #[test]
    fn url_parser_rejects_credentials_unsupported_schemes_and_ambiguous_hosts() {
        assert!(parse_http_url("ftp://example.com/file").is_err());
        assert!(parse_http_url("http://user@example.com/").is_err());
        assert!(parse_http_url("http://2001:db8::1/").is_err());
        assert!(parse_http_url("http://2130706433/").is_err());
        assert!(parse_http_url("http://0177.0.0.1/").is_err());
        assert!(parse_http_url("http://0x7f000001/").is_err());
        assert!(parse_http_url("http://example.com:0/").is_err());
        assert_eq!(
            parse_http_url("https://[2606:4700:4700::1111]/path").unwrap(),
            Endpoint {
                scheme: "https".to_owned(),
                host: "2606:4700:4700::1111".to_owned(),
                port: 443,
            }
        );
    }

    #[test]
    fn redirects_must_be_revalidated_against_each_resolved_address_set() {
        let allowlist = HashSet::new();
        let endpoint = Endpoint {
            scheme: "https".to_owned(),
            host: "example.test".to_owned(),
            port: 443,
        };
        let mut answer = 0;
        let mut resolver = |_: &Endpoint| {
            answer += 1;
            Ok(vec![if answer == 1 {
                "93.184.216.34".parse().unwrap()
            } else {
                "10.0.0.7".parse().unwrap()
            }])
        };
        assert!(resolve_and_validate_with(&endpoint, &allowlist, &mut resolver).is_ok());
        assert!(resolve_and_validate_with(&endpoint, &allowlist, &mut resolver).is_err());
    }

    #[test]
    fn dns_result_wait_respects_the_supplied_deadline() {
        let (sender, receiver) = channel::<std::result::Result<Vec<IpAddr>, String>>();
        let expired_deadline = Instant::now() - Duration::from_secs(1);
        let error = receive_dns_result(receiver, expired_deadline).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("exceeded the URL fetch deadline")
        );
        assert!(sender.send(Ok(Vec::new())).is_err());
    }

    #[test]
    fn mixed_dns_answers_fail_unless_each_private_ip_is_explicitly_trusted() {
        let endpoint = Endpoint {
            scheme: "https".to_owned(),
            host: "example.test".to_owned(),
            port: 443,
        };
        let answers = [
            "93.184.216.34".parse().unwrap(),
            "10.0.0.7".parse().unwrap(),
        ];
        assert!(validate_addresses(&endpoint, &answers, &HashSet::new()).is_err());

        let trusted = ["10.0.0.7".parse().unwrap()].into_iter().collect();
        assert!(validate_addresses(&endpoint, &answers, &trusted).is_ok());
        let unrelated = ["10.0.0.8".parse().unwrap()].into_iter().collect();
        assert!(validate_addresses(&endpoint, &answers, &unrelated).is_err());
    }

    #[test]
    fn resolve_entries_pin_every_address_and_format_ipv6() {
        let endpoint = Endpoint {
            scheme: "https".to_owned(),
            host: "example.com".to_owned(),
            port: 443,
        };
        assert_eq!(
            resolve_entries(
                &endpoint,
                &[
                    "93.184.216.34".parse().unwrap(),
                    "2606:4700:4700::1111".parse().unwrap(),
                ]
            ),
            [
                "example.com:443:93.184.216.34",
                "example.com:443:[2606:4700:4700::1111]"
            ]
        );
    }
}
