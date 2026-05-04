use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// GitHub repo-collaborator permission level.
/// Stored as the lowercase string GitHub returns (`"pull"`, `"push"`, etc.).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Pull,
    Triage,
    Push,
    Maintain,
    Admin,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("unknown permission: {0}")]
pub struct UnknownPermission(pub String);

impl FromStr for Permission {
    type Err = UnknownPermission;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pull" => Ok(Self::Pull),
            "triage" => Ok(Self::Triage),
            "push" => Ok(Self::Push),
            "maintain" => Ok(Self::Maintain),
            "admin" => Ok(Self::Admin),
            other => Err(UnknownPermission(other.to_string())),
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pull => "pull",
            Self::Triage => "triage",
            Self::Push => "push",
            Self::Maintain => "maintain",
            Self::Admin => "admin",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_levels() {
        assert_eq!("pull".parse(), Ok(Permission::Pull));
        assert_eq!("triage".parse(), Ok(Permission::Triage));
        assert_eq!("push".parse(), Ok(Permission::Push));
        assert_eq!("maintain".parse(), Ok(Permission::Maintain));
        assert_eq!("admin".parse(), Ok(Permission::Admin));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(
            "owner".parse::<Permission>(),
            Err(UnknownPermission("owner".into()))
        );
    }

    #[test]
    fn display_round_trips() {
        for p in [
            Permission::Pull,
            Permission::Triage,
            Permission::Push,
            Permission::Maintain,
            Permission::Admin,
        ] {
            assert_eq!(p.to_string().parse::<Permission>().unwrap(), p);
        }
    }
}
