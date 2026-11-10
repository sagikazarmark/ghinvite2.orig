//! Collect real invocation metadata and retained journals for a fixed step table.
use std::collections::BTreeMap;

use restate_e2e_harness::{Invocation, JournalEntry, Reply, RunPath, Table, Walked};

use crate::{Call, Harness};

#[derive(Default)]
pub struct JournalAudit {
    invocations: Vec<(String, Invocation)>,
    journals: BTreeMap<String, Vec<JournalEntry>>,
}

impl JournalAudit {
    /// Set retention before executing the scenario. Empty named-run sequences
    /// still require a real, nonempty journal, never a missing SQL result.
    pub async fn retain(harness: &Harness, services: &[&str]) {
        for service in services {
            harness
                .client
                .patch(format!(
                    "{}/services/{service}",
                    harness.restate.admin_url()
                ))
                .json(&serde_json::json!({"journal_retention":"1h","idempotency_retention":"1h"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
    }

    pub async fn invoke(
        &mut self,
        harness: &Harness,
        service: &str,
        key: &str,
        handler: &str,
        body: &serde_json::Value,
    ) -> Reply {
        let reply = harness
            .restate
            .invoke(
                &Call::object(service, key, handler),
                (!body.is_null()).then_some(body),
                None,
            )
            .await;
        let id = reply.invocation_id().to_owned();
        self.capture(harness, &id).await;
        reply
    }

    /// Retain a stateless service call under the same strict journal checks.
    pub async fn invoke_service(
        &mut self,
        harness: &Harness,
        service: &str,
        handler: &str,
        body: &serde_json::Value,
    ) -> Reply {
        let reply = harness
            .restate
            .invoke(
                &Call::service(service, handler),
                (!body.is_null()).then_some(body),
                None,
            )
            .await;
        let id = reply.invocation_id().to_owned();
        self.capture(harness, &id).await;
        reply
    }

    /// Include an actual one-way invocation after its handler completed.
    pub async fn capture(&mut self, harness: &Harness, id: &str) {
        let invocation = harness.restate.admin().invocation(id).await;
        let journal = harness.restate.admin().journal(id).await;
        assert!(!journal.is_empty(), "retained journal missing: {id}");
        self.invocations.push((id.to_owned(), invocation));
        assert!(
            self.journals.insert(id.to_owned(), journal).is_none(),
            "scenario reused an invocation ID"
        );
    }

    /// Discover all handlers of the explicitly scoped services. A newly added
    /// handler fails even when the scenario has never called it.
    pub async fn check(
        &self,
        harness: &Harness,
        services: &[&str],
        paths: &'static [RunPath],
    ) -> Walked {
        let handlers = harness
            .restate
            .admin()
            .handlers()
            .await
            .into_iter()
            .filter(|handler| services.contains(&handler.service.as_str()))
            .collect::<Vec<_>>();
        let table = Table::new(paths);
        let walked = table
            .check(&handlers, &self.invocations, &self.journals)
            .unwrap_or_else(|violations| panic!("{violations}"));
        // A deleted retained journal must be detected.
        let mut incomplete = self.journals.clone();
        incomplete.remove(&self.invocations[0].0);
        assert!(
            table
                .check(&handlers, &self.invocations, &incomplete)
                .is_err()
        );
        walked
    }
}
