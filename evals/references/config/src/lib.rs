use std::fmt;

/// Connection settings read from a small `key = value` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub retries: u32,
}

/// Why a configuration file was rejected. Line numbers are 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Syntax { line: usize },
    UnknownKey { line: usize, key: String },
    DuplicateKey { line: usize, key: String },
    InvalidValue { line: usize, key: String, value: String },
    MissingKey(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line } => write!(f, "line {line}: expected key = value"),
            Self::UnknownKey { line, key } => write!(f, "line {line}: unknown key {key:?}"),
            Self::DuplicateKey { line, key } => write!(f, "line {line}: duplicate key {key:?}"),
            Self::InvalidValue { line, key, value } => {
                write!(f, "line {line}: invalid value {value:?} for {key}")
            }
            Self::MissingKey(key) => write!(f, "missing required key {key:?}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parses `key = value` lines. Blank lines and lines starting with `#` are ignored.
pub fn parse_config(text: &str) -> Result<Config, ConfigError> {
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut retries: Option<u32> = None;
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(ConfigError::Syntax { line: line_no })?;
        let key = key.trim();
        let value = value.trim();
        let invalid = || ConfigError::InvalidValue {
            line: line_no,
            key: key.to_string(),
            value: value.to_string(),
        };
        let already_set = match key {
            "host" => host.is_some(),
            "port" => port.is_some(),
            "retries" => retries.is_some(),
            _ => {
                return Err(ConfigError::UnknownKey {
                    line: line_no,
                    key: key.to_string(),
                });
            }
        };
        if already_set {
            return Err(ConfigError::DuplicateKey {
                line: line_no,
                key: key.to_string(),
            });
        }
        match key {
            "host" if value.is_empty() => return Err(invalid()),
            "host" => host = Some(value.to_string()),
            "port" => match value.parse::<u16>() {
                Ok(p) if p >= 1 => port = Some(p),
                _ => return Err(invalid()),
            },
            _ => retries = Some(value.parse().map_err(|_| invalid())?),
        }
    }
    Ok(Config {
        host: host.ok_or_else(|| ConfigError::MissingKey("host".to_string()))?,
        port: port.ok_or_else(|| ConfigError::MissingKey("port".to_string()))?,
        retries: retries.unwrap_or(3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_complete_file() {
        let config =
            parse_config("# service\nhost = example.org\nport = 8080\n\nretries = 5\n").unwrap();
        assert_eq!(
            config,
            Config {
                host: "example.org".to_string(),
                port: 8080,
                retries: 5,
            }
        );
    }

    #[test]
    fn reports_errors() {
        assert_eq!(parse_config("oops"), Err(ConfigError::Syntax { line: 1 }));
        assert_eq!(
            parse_config("host = a\nport = 0"),
            Err(ConfigError::InvalidValue {
                line: 2,
                key: "port".to_string(),
                value: "0".to_string(),
            })
        );
        assert_eq!(
            parse_config("host = a"),
            Err(ConfigError::MissingKey("port".to_string()))
        );
    }
}
