use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// A release version made of three numeric components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns the next version that is compatible for a bug fix.
    pub const fn bump_patch(self) -> Self {
        Self::new(self.major, self.minor, self.patch + 1)
    }
}

/// Returned when text is not a MAJOR.MINOR.PATCH version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError;

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid version: expected MAJOR.MINOR.PATCH")
    }
}

impl std::error::Error for ParseVersionError {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn component(text: &str) -> Result<u64, ParseVersionError> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || (text.len() > 1 && text.starts_with('0')) {
        return Err(ParseVersionError);
    }
    text.parse().map_err(|_| ParseVersionError)
}

impl FromStr for Version {
    type Err = ParseVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = text.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            return Err(ParseVersionError);
        };
        Ok(Self::new(component(major)?, component(minor)?, component(patch)?))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_patch_keeps_major_and_minor() {
        assert_eq!(Version::new(1, 2, 3).bump_patch(), Version::new(1, 2, 4));
    }

    #[test]
    fn round_trip_and_errors() {
        assert_eq!("1.20.3".parse::<Version>().unwrap().to_string(), "1.20.3");
        assert!("01.2.3".parse::<Version>().is_err());
        assert!("+1.2.3".parse::<Version>().is_err());
        assert!(Version::new(1, 10, 0) > Version::new(1, 9, 0));
    }
}
