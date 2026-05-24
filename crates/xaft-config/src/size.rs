//! Human-friendly byte size parsing (e.g. "10MB", "512KB").

use crate::error::ConfigError;

/// Parse a human-friendly size string into bytes.
///
/// Supported suffixes (case-insensitive): `B`, `KB`, `MB`, `GB`, `TB`.
/// If no suffix is given, the value is treated as bytes.
///
/// # Examples
///
/// ```
/// use xaft_config::size::parse_size;
/// assert_eq!(parse_size("1KB").unwrap(), 1024);
/// assert_eq!(parse_size("10MB").unwrap(), 10 * 1024 * 1024);
/// assert_eq!(parse_size("2.5GB").unwrap(), (2.5 * 1024.0 * 1024.0 * 1024.0) as u64);
/// ```
pub fn parse_size(s: &str) -> Result<u64, ConfigError> {
    let s = s.trim();
    let upper = s.to_uppercase();

    let (num_str, multiplier): (&str, u64) = if upper.ends_with("TB") {
        (&s[..s.len() - 2], 1_099_511_627_776)
    } else if upper.ends_with("GB") {
        (&s[..s.len() - 2], 1_073_741_824)
    } else if upper.ends_with("MB") {
        (&s[..s.len() - 2], 1_048_576)
    } else if upper.ends_with("KB") {
        (&s[..s.len() - 2], 1_024)
    } else if upper.ends_with('B') {
        (&s[..s.len() - 1], 1)
    } else {
        (s, 1)
    };

    let num: f64 = num_str
        .trim()
        .parse()
        .map_err(|_| ConfigError::InvalidSize(s.to_string()))?;

    if num < 0.0 {
        return Err(ConfigError::InvalidSize(format!(
            "{s}: size must not be negative"
        )));
    }

    Ok((num * multiplier as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kilobytes() {
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("1kb").unwrap(), 1024);
    }

    #[test]
    fn parse_megabytes() {
        assert_eq!(parse_size("10MB").unwrap(), 10 * 1024 * 1024);
    }

    #[test]
    fn parse_gigabytes() {
        assert_eq!(parse_size("1GB").unwrap(), 1_073_741_824);
    }

    #[test]
    fn parse_bytes() {
        assert_eq!(parse_size("512B").unwrap(), 512);
        assert_eq!(parse_size("512").unwrap(), 512);
    }

    #[test]
    fn parse_fractional() {
        let result = parse_size("1.5MB").unwrap();
        assert_eq!(result, (1.5 * 1_048_576.0) as u64);
    }

    #[test]
    fn parse_invalid_returns_error() {
        assert!(parse_size("not_a_size").is_err());
        assert!(parse_size("MB").is_err());
    }

    #[test]
    fn parse_negative_returns_error() {
        assert!(parse_size("-1MB").is_err());
    }

    #[test]
    fn parse_whitespace_trimmed() {
        assert_eq!(parse_size("  10 MB  ").unwrap(), 10 * 1024 * 1024);
    }
}
