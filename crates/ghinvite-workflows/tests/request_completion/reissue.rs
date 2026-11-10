use super::*;
use ghinvite_e2e::Call;
impl Rig {
    async fn start_reissue() -> Self {
        let h = Harness::start().await;
        let link_clock = Arc::new(ghinvite_workflows::admission::Faults::default());
        let request_clock = Arc::new(ghinvite_workflows::request_owner::Faults::default());
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let state = ghinvite_workflows::AppState::new(storage.clone());
        let builder = ghinvite_workflows::admission::bind_with_faults(
            restate_sdk::endpoint::Endpoint::builder(),
            link_clock.clone(),
        );
        let builder = ghinvite_workflows::request_owner::bind_with_reissue_faults(
            builder,
            request_clock.clone(),
            false,
        );
        let builder = ghinvite_workflows::invite_budget::bind_pre_activation(
            builder,
            storage.clone(),
            Default::default(),
        );
        let builder = ghinvite_workflows::access_grant::bind(builder, state, storage);
        h.restate
            .deploy(
                builder
                    .bind(AccountInstallation)
                    .bind(RepositoryDelivery)
                    .bind(InvitationProjection)
                    .bind(RequestProjection)
                    .build(),
            )
            .await;
        Self {
            h,
            link_clock,
            request_clock,
        }
    }
    async fn reissue(&self, id: &str, input: &Value) -> Value {
        let reply = self
            .h
            .restate
            .invoke(
                &Call::object("InvitationRequest", id, "reissue"),
                Some(input),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        let journal = self.h.restate.admin().journal(reply.invocation_id()).await;
        assert!(!journal.is_empty());
        let names: Vec<_> = journal
            .iter()
            .filter(|e| e.is_run())
            .map(|e| e.name.as_deref().unwrap())
            .collect();
        const PATHS: &[&[&str]] = &[
            &[],
            &["reissue_time"],
            &["reissue_time", "after-reissue-epoch"],
        ];
        assert!(
            PATHS.contains(&names.as_slice()),
            "unreviewed request reissue path: {names:?}"
        );
        reply.body
    }
    async fn wait_generation(&self, id: &str, repo: u64, generation: u64) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let value = self
                    .call(
                        "RepositoryDelivery",
                        format!("{id}:{repo}:{generation}"),
                        "status",
                        &Value::Null,
                    )
                    .await;
                if !value.is_null() {
                    break value;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
fn command(request: &str, repo: u64) -> Value {
    json!({"operation_id":RequestId::new(),"request_id":request,"admin":{"account_id":100,"user_id":7},"target":{"kind":"repository","repo_id":repo}})
}
#[tokio::test]
async fn retained_reissue_epochs_preserve_original_approval_and_admission_receipts() {
    let r = Rig::start_reissue().await;
    let (link, admit, receipt) = r.fixture(604800, &[10]).await;
    let id = receipt["result"]["request_id"].as_str().unwrap();
    let original = r.request(link, id).await;
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"expired","reason":null}),
    )
    .await;
    r.wait_state(link, id, "lapsed").await;
    let first = command(id, 10);
    let result = r.reissue(id, &first).await;
    assert_eq!(result, json!({"Ok":{"delivery_generation":2}}));
    let created = r.wait_generation(id, 10, 2).await;
    assert_eq!(
        created["create"]["command"]["budget_attribution"],
        "administrator_reissue"
    );
    assert_eq!(created["create"]["command"]["link_id"], json!(link));
    let current = r.wait_state(link, id, "approved").await;
    assert_eq!(current["decision"], original["decision"]);
    assert_eq!(r.reissue(id, &first).await, result);
    assert_eq!(
        r.call("InvitationLink", link, "admit", &admit).await,
        receipt
    );
    let progress = r
        .call(
            "InvitationRequest",
            id,
            "delivery_progress",
            &json!({"request_id":id,"link_id":link,"requester_id":8}),
        )
        .await;
    assert_eq!(progress[0]["delivery_generation"], 2);
    let mut changed = first.clone();
    changed["target"]["repo_id"] = json!(11);
    assert_eq!(
        r.h.restate
            .invoke(
                &Call::object("InvitationRequest", id, "reissue"),
                Some(&changed),
                None
            )
            .await
            .status,
        409
    );
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:2"),
        "finish",
        &json!({"state":"expired","reason":null}),
    )
    .await;
    r.wait_state(link, id, "lapsed").await;
    assert_eq!(r.reissue(id, &first).await, result);
    assert_eq!(
        r.reissue(id, &command(id, 10)).await,
        json!({"Ok":{"delivery_generation":3}})
    );
    r.wait_generation(id, 10, 3).await;
    // Late generation1 completion must not put the current epoch back to Lapsed.
    r.call(
        "InvitationRequest",
        id,
        "on_delivery_settled",
        &json!({"kind":"repository","target_id":10,"delivery_generation":1}),
    )
    .await;
    assert_eq!(r.request(link, id).await["state"], "approved");
    r.h.finish().await;
}
#[tokio::test]
async fn retained_refusal_is_exact_and_pending_old_effects_never_dispatch() {
    let r = Rig::start_reissue().await;
    let (link, _, receipt) = r.fixture(604800, &[10]).await;
    let id = receipt["result"]["request_id"].as_str().unwrap();
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"expired","reason":null}),
    )
    .await;
    r.wait_state(link, id, "lapsed").await;
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "set_barrier",
        &json!("may_have_written"),
    )
    .await;
    let input = command(id, 10);
    let refusal = r.reissue(id, &input).await;
    assert_eq!(refusal["Err"]["kind"], "not_reissuable");
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "set_barrier",
        &json!("clear"),
    )
    .await;
    assert_eq!(r.reissue(id, &input).await, refusal);
    assert!(
        r.call(
            "RepositoryDelivery",
            format!("{id}:10:2"),
            "status",
            &Value::Null
        )
        .await
        .is_null()
    );
    let mut unauthorized = command(id, 10);
    unauthorized["admin"]["user_id"] = json!(8);
    assert_eq!(
        r.h.restate
            .invoke(
                &Call::object("InvitationRequest", id, "reissue"),
                Some(&unauthorized),
                None
            )
            .await
            .status,
        404
    );
    assert_eq!(
        r.reissue(id, &command(id, 10)).await,
        json!({"Ok":{"delivery_generation":2}})
    );
    r.h.finish().await;
}

#[tokio::test]
async fn killed_reissue_retains_one_atomic_epoch_and_recovers_same_generation() {
    use std::sync::atomic::Ordering;
    let r = Rig::start_reissue().await;
    let (link, _, receipt) = r.fixture(604800, &[10]).await;
    let id = receipt["result"]["request_id"].as_str().unwrap();
    r.call(
        "RepositoryDelivery",
        format!("{id}:10:1"),
        "finish",
        &json!({"state":"expired","reason":null}),
    )
    .await;
    let before = r.wait_state(link, id, "lapsed").await;
    *r.request_clock.stage.lock().unwrap() = Some("after-reissue-epoch".into());
    let input = command(id, 10);
    let sent =
        r.h.restate
            .invoke(
                &Call::object("InvitationRequest", id, "reissue").send(),
                Some(&input),
                None,
            )
            .await;
    assert_eq!(sent.status, 202);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while r.request_clock.reached.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let raw:Value=r.h.client.post(format!("{}/query",r.h.restate.admin_url())).header("accept","application/json").json(&json!({"query":format!("SELECT key,value_utf8 FROM state WHERE service_name='InvitationRequest' AND service_key='{id}'")})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let epoch: Value = serde_json::from_str(
        raw["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == "reissue/current")
            .unwrap()["value_utf8"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(epoch["pending"], true);
    assert_eq!(epoch["proof"]["before"], before);
    assert_eq!(epoch["proof"]["after"], epoch["plan"]["input"]["request"]);
    assert_eq!(epoch["receipt"]["input"], input);
    assert_eq!(epoch["receipt"]["outcome"]["Ok"]["delivery_generation"], 2);
    assert!(
        r.call(
            "RepositoryDelivery",
            format!("{id}:10:2"),
            "status",
            &Value::Null
        )
        .await
        .is_null()
    );
    r.h.restate.admin().kill(sent.invocation_id()).await;
    r.h.restate
        .admin()
        .await_status(sent.invocation_id(), &["completed"])
        .await;
    *r.request_clock.stage.lock().unwrap() = None;
    assert_eq!(r.reissue(id, &input).await, epoch["receipt"]["outcome"]);
    let created = r.wait_generation(id, 10, 2).await;
    assert_eq!(created["create"]["command"], epoch["plan"]["commands"][0]);
    assert_eq!(r.request(link, id).await, epoch["proof"]["after"]);
    assert!(
        r.call(
            "RepositoryDelivery",
            format!("{id}:10:3"),
            "status",
            &Value::Null
        )
        .await
        .is_null()
    );
    r.h.finish().await;
}
