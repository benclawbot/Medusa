use std::{cmp::Ordering, fmt, str::FromStr};

use semver::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The user-facing release identity. Cargo package versions remain SemVer;
/// the optional fourth component distinguishes rebuilds of the same package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseId {
    version: Version,
    iteration: u64,
    explicit_iteration: bool,
}

impl ReleaseId {
    pub fn parse(value: &str) -> Result<Self, String> {
        value.parse()
    }

    #[must_use]
    pub fn from_version(version: &Version) -> Self {
        Self {
            version: version.clone(),
            iteration: 0,
            explicit_iteration: false,
        }
    }

    #[must_use]
    pub fn base_version(&self) -> &Version {
        &self.version
    }
}

impl FromStr for ReleaseId {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim().trim_start_matches('v');
        let components = value.split('.').collect::<Vec<_>>();
        match components.len() {
            3 => Ok(Self {
                version: Version::parse(value).map_err(|error| error.to_string())?,
                iteration: 0,
                explicit_iteration: false,
            }),
            4 => {
                let iteration = components[3]
                    .parse::<u64>()
                    .map_err(|error| format!("invalid release iteration: {error}"))?;
                let base = components[..3].join(".");
                Ok(Self {
                    version: Version::parse(&base).map_err(|error| error.to_string())?,
                    iteration,
                    explicit_iteration: true,
                })
            }
            _ => Err(
                "release identity must contain three or four dot-separated components".to_owned(),
            ),
        }
    }
}

impl Ord for ReleaseId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.version
            .cmp(&other.version)
            .then(self.iteration.cmp(&other.iteration))
    }
}

impl PartialOrd for ReleaseId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ReleaseId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.explicit_iteration {
            write!(formatter, "{}.{}", self.version, self.iteration)
        } else {
            self.version.fmt(formatter)
        }
    }
}

impl Serialize for ReleaseId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ReleaseId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)
            .and_then(|value| Self::parse(&value).map_err(serde::de::Error::custom))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_part_release_ids_order_after_their_base_package() {
        let base = ReleaseId::parse("1.0.7").expect("base");
        let rebuild = ReleaseId::parse("v1.0.7.1").expect("rebuild");
        let next = ReleaseId::parse("1.0.8").expect("next");
        assert!(base < rebuild);
        assert!(rebuild < next);
        assert_eq!(rebuild.to_string(), "1.0.7.1");
    }
}
