use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cached GitHub user info. `login` is mutable upstream; refreshed on each sign-in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub user_id: u64,
    pub login: String,
    pub avatar_url: Option<String>,
    pub last_seen_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn user_serde_round_trips() {
        let u = User {
            user_id: 42,
            login: "octocat".into(),
            avatar_url: Some("https://example.test/a.png".into()),
            last_seen_at: Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap(),
        };
        let s = serde_json::to_string(&u).unwrap();
        let parsed: User = serde_json::from_str(&s).unwrap();
        assert_eq!(u, parsed);
    }
}
