//! Compact tunnel specs that mirror OpenSSH `-L` / `-R` / `-D` flags.
//!
//! Forms: `L:localport:host:port`, `R:remoteport:host:port`, `D:port`.

use std::fmt;

/// A parsed port-forward request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelSpec {
    /// Local forward: listen on the client, connect through the remote.
    Local {
        /// Port on the local machine.
        local_port: u16,
        /// Host as seen from the remote side (often `localhost`).
        host: String,
        /// Port on `host`.
        port: u16,
    },
    /// Remote forward: listen on the remote, connect back to the client side.
    Remote {
        /// Port on the remote machine.
        remote_port: u16,
        /// Host as seen from the client (often `localhost`).
        host: String,
        /// Port on `host`.
        port: u16,
    },
    /// Dynamic SOCKS proxy on a local port.
    Dynamic {
        /// Local SOCKS listen port.
        port: u16,
    },
}

/// Why [`TunnelSpec::parse`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Human-readable reason.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl TunnelSpec {
    /// Parse a compact tunnel string.
    pub fn parse(raw: &str) -> Result<Self, ParseError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ParseError {
                message: "empty tunnel spec".into(),
            });
        }
        let (kind, rest) = raw
            .split_once(':')
            .ok_or_else(|| ParseError {
                message: format!(
                    "invalid tunnel spec `{raw}`; expected L:local:host:port, R:remote:host:port, or D:port"
                ),
            })?;
        match kind {
            "L" | "l" => parse_forward(rest, true),
            "R" | "r" => parse_forward(rest, false),
            "D" | "d" => {
                let port = parse_port(rest, "dynamic port")?;
                Ok(Self::Dynamic { port })
            }
            other => Err(ParseError {
                message: format!(
                    "unknown tunnel kind `{other}`; use L (local), R (remote), or D (dynamic)"
                ),
            }),
        }
    }

    /// OpenSSH flag form, for example `-L 3000:localhost:3000`.
    pub fn to_ssh_flag(&self) -> String {
        match self {
            Self::Local {
                local_port,
                host,
                port,
            } => format!("-L {local_port}:{host}:{port}"),
            Self::Remote {
                remote_port,
                host,
                port,
            } => format!("-R {remote_port}:{host}:{port}"),
            Self::Dynamic { port } => format!("-D {port}"),
        }
    }

    /// Local listen port for local/dynamic forwards (used for bind checks).
    pub fn local_listen_port(&self) -> Option<u16> {
        match self {
            Self::Local { local_port, .. } => Some(*local_port),
            Self::Dynamic { port } => Some(*port),
            Self::Remote { .. } => None,
        }
    }

    /// Compact form matching the inventory / ad-hoc input.
    pub fn to_compact(&self) -> String {
        match self {
            Self::Local {
                local_port,
                host,
                port,
            } => format!("L:{local_port}:{host}:{port}"),
            Self::Remote {
                remote_port,
                host,
                port,
            } => format!("R:{remote_port}:{host}:{port}"),
            Self::Dynamic { port } => format!("D:{port}"),
        }
    }
}

fn parse_forward(rest: &str, local: bool) -> Result<TunnelSpec, ParseError> {
    let parts: Vec<&str> = rest.splitn(3, ':').collect();
    if parts.len() != 3 {
        let kind = if local { "L" } else { "R" };
        return Err(ParseError {
            message: format!(
                "invalid {kind} tunnel `{kind}:{rest}`; expected {kind}:port:host:port"
            ),
        });
    }
    let listen = parse_port(parts[0], if local { "local port" } else { "remote port" })?;
    let host = parts[1].trim();
    if host.is_empty() {
        return Err(ParseError {
            message: "tunnel host must not be empty".into(),
        });
    }
    let port = parse_port(parts[2], "destination port")?;
    if local {
        Ok(TunnelSpec::Local {
            local_port: listen,
            host: host.to_string(),
            port,
        })
    } else {
        Ok(TunnelSpec::Remote {
            remote_port: listen,
            host: host.to_string(),
            port,
        })
    }
}

fn parse_port(raw: &str, label: &str) -> Result<u16, ParseError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ParseError {
            message: format!("{label} is missing"),
        });
    }
    raw.parse::<u16>().map_err(|_| ParseError {
        message: format!("{label} `{raw}` is not a valid port (1–65535)"),
    }).and_then(|port| {
        if port == 0 {
            Err(ParseError {
                message: format!("{label} must not be 0"),
            })
        } else {
            Ok(port)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_forward() {
        let spec = TunnelSpec::parse("L:3000:localhost:3000").unwrap();
        assert_eq!(
            spec,
            TunnelSpec::Local {
                local_port: 3000,
                host: "localhost".into(),
                port: 3000,
            }
        );
        assert_eq!(spec.to_ssh_flag(), "-L 3000:localhost:3000");
        assert_eq!(spec.local_listen_port(), Some(3000));
    }

    #[test]
    fn parses_remote_forward() {
        let spec = TunnelSpec::parse("R:9000:127.0.0.1:9000").unwrap();
        assert_eq!(
            spec,
            TunnelSpec::Remote {
                remote_port: 9000,
                host: "127.0.0.1".into(),
                port: 9000,
            }
        );
        assert_eq!(spec.to_ssh_flag(), "-R 9000:127.0.0.1:9000");
        assert_eq!(spec.local_listen_port(), None);
    }

    #[test]
    fn parses_dynamic() {
        let spec = TunnelSpec::parse("D:1080").unwrap();
        assert_eq!(spec, TunnelSpec::Dynamic { port: 1080 });
        assert_eq!(spec.to_ssh_flag(), "-D 1080");
        assert_eq!(spec.to_compact(), "D:1080");
    }

    #[test]
    fn accepts_lowercase_kind() {
        assert!(TunnelSpec::parse("l:1:h:2").is_ok());
        assert!(TunnelSpec::parse("r:1:h:2").is_ok());
        assert!(TunnelSpec::parse("d:9").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(TunnelSpec::parse("").unwrap_err().message.contains("empty"));
    }

    #[test]
    fn rejects_unknown_kind() {
        let err = TunnelSpec::parse("X:1:h:2").unwrap_err();
        assert!(err.message.contains("unknown tunnel kind"));
    }

    #[test]
    fn rejects_bad_local_shape() {
        let err = TunnelSpec::parse("L:3000:localhost").unwrap_err();
        assert!(err.message.contains("expected L:port:host:port"));
    }

    #[test]
    fn rejects_port_zero() {
        assert!(
            TunnelSpec::parse("D:0")
                .unwrap_err()
                .message
                .contains("must not be 0")
        );
    }

    #[test]
    fn rejects_non_numeric_port() {
        assert!(
            TunnelSpec::parse("D:abc")
                .unwrap_err()
                .message
                .contains("not a valid port")
        );
    }

    #[test]
    fn rejects_empty_host() {
        assert!(
            TunnelSpec::parse("L:1::2")
                .unwrap_err()
                .message
                .contains("host must not be empty")
        );
    }

    #[test]
    fn round_trips_compact_form() {
        for raw in ["L:3000:localhost:3000", "R:9:db:5432", "D:1080"] {
            let spec = TunnelSpec::parse(raw).unwrap();
            assert_eq!(spec.to_compact(), raw);
            assert_eq!(TunnelSpec::parse(&spec.to_compact()).unwrap(), spec);
        }
    }
}
