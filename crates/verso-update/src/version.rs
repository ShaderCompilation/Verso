//! Release versions: `MAJOR.MINOR.PATCH` with an optional `-pre.release`
//! suffix, ordered the way Semantic Versioning orders them.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{cmp::Ordering, fmt, str::FromStr};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release identifiers (`rc.1` in `0.3.0-rc.1`), empty
    /// for a release.
    pub pre: Vec<String>,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch, pre: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseVersionError(String);

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a version like 0.3.0", self.0)
    }
}

impl std::error::Error for ParseVersionError {}

impl FromStr for Version {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseVersionError(s.to_string());
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let number = |part: &str| -> Result<u64, ParseVersionError> {
            // No leading zeros or signs, so every version has one spelling.
            if part.is_empty() || (part.len() > 1 && part.starts_with('0')) || !part.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(err());
            }
            part.parse().map_err(|_| err())
        };
        let mut parts = core.split('.');
        let (Some(major), Some(minor), Some(patch), None) = (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(err());
        };
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_string).collect();
                let valid = |id: &String| {
                    !id.is_empty()
                        && id.bytes().all(|b| b.is_ascii_alphanumeric())
                        && !(id.len() > 1 && id.starts_with('0') && id.bytes().all(|b| b.is_ascii_digit()))
                };
                if !ids.iter().all(valid) {
                    return Err(err());
                }
                ids
            }
        };
        Ok(Version { major: number(major)?, minor: number(minor)?, patch: number(patch)?, pre })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch)).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A pre-release comes before its release.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => cmp_pre(&self.pre, &other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            // Numeric identifiers come before alphanumeric ones.
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(v("0.3.0"), Version::new(0, 3, 0));
        assert_eq!(v("10.20.30").to_string(), "10.20.30");
        assert_eq!(v("1.0.0-rc.1").pre, ["rc", "1"]);
        assert_eq!(v("1.0.0-rc.1").to_string(), "1.0.0-rc.1");
        for bad in
            ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "+1.2.3", "1.2.3-", "1.2.3-a..b", "1.2.3-01", "v1.2.3"]
        {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn orders_like_semver() {
        let ordered = [
            "0.1.0",
            "0.1.1",
            "0.2.0",
            "0.10.0",
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "2.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("0.3.0").cmp(&v("0.3.0")), Ordering::Equal);
    }

    #[test]
    fn serde_is_a_string() {
        assert_eq!(serde_json::to_string(&v("0.3.0")).unwrap(), "\"0.3.0\"");
        assert_eq!(serde_json::from_str::<Version>("\"0.3.0-rc.2\"").unwrap(), v("0.3.0-rc.2"));
        assert!(serde_json::from_str::<Version>("\"three\"").is_err());
    }
}
