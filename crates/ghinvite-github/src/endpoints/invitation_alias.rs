//! A numeric invitation-list alias is accepted only from authenticated first-page facts.
//! GitHub documents repository.id in each invitation; no extra identity read is made.
use crate::{rest::PathTemplate, transport::Response};
use parking_lot::Mutex;
use serde::Deserialize;

pub(super) struct Identity {
    enabled: bool,
    named_path: String,
    base_path: String,
    state: Mutex<State>,
}
#[derive(Default)]
struct State {
    observed: bool,
    alias: Option<String>,
}
impl Identity {
    pub(super) fn new(enabled: bool, named_path: &str, base_path: &str) -> Self {
        Self {
            enabled,
            named_path: named_path.to_owned(),
            base_path: base_path.trim_end_matches('/').to_owned(),
            state: Mutex::new(State::default()),
        }
    }
    pub(super) fn matches(&self, path: &str) -> bool {
        self.enabled && self.state.lock().alias.as_deref() == Some(path)
    }
    pub(super) fn observe_first_page(&self, response: &Response) {
        if !self.enabled {
            return;
        }
        let mut state = self.state.lock();
        if state.observed {
            return;
        }
        state.observed = true;
        state.alias = self
            .first_page_identity(&response.body)
            .map(|id| format!("{}/repositories/{id}/invitations", self.base_path));
    }
    fn first_page_identity(&self, bytes: &[u8]) -> Option<u64> {
        #[derive(Deserialize)]
        struct Row {
            repository: Repository,
        }
        #[derive(Deserialize)]
        struct Repository {
            id: u64,
            full_name: Option<String>,
        }
        let rows: Vec<Row> = serde_json::from_slice(bytes).ok()?;
        let id = rows.first()?.repository.id;
        if id == 0 {
            return None;
        }
        for row in rows {
            if row.repository.id != id {
                return None;
            }
            if let Some(full_name) = row.repository.full_name {
                let (owner, name) = full_name.split_once('/')?;
                let path = PathTemplate::new("/repos/{}/{}/invitations")
                    .seg(owner)
                    .seg(name)
                    .build()
                    .ok()?;
                if !path.eq_ignore_ascii_case(&self.named_path) {
                    return None;
                }
            }
        }
        Some(id)
    }
}
