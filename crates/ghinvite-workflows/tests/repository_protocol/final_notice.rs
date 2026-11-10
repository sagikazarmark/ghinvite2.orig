//! Recording consumer for isolated owner tests; real request aggregation has a
//! separate standard-endpoint test. The consumer reads durable owner status.
use super::*;
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json as RestateJson,
};
pub struct InvitationRequest;
#[restate_sdk::object]
impl InvitationRequest {
    #[handler]
    async fn on_delivery_settled(
        &self,
        ctx: ObjectContext<'_>,
        RestateJson(notice): RestateJson<ghinvite_core::request_completion::DeliveryNotice>,
    ) -> Result<(), TerminalError> {
        assert_eq!(
            notice.kind,
            ghinvite_core::delivery_control::DeliveryKind::Repository
        );
        let snapshot = ctx
            .object_client::<ghinvite_workflows::delivery::RepositoryDeliveryClient>(format!(
                "{}:{}:{}",
                ctx.key(),
                notice.target_id,
                notice.delivery_generation
            ))
            .status()
            .call()
            .await?
            .0
            .expect("notice follows durable snapshot");
        assert_eq!(snapshot.create.command.request_id.to_string(), ctx.key());
        assert_eq!(snapshot.create.command.repo_id, notice.target_id);
        assert_eq!(
            snapshot.create.command.delivery_generation,
            notice.delivery_generation
        );
        let result = ghinvite_core::request_completion::repository(&snapshot)
            .expect("only final snapshots notify");
        ctx.set(
            "observed",
            RestateJson(json!({"notice":notice,"snapshot":snapshot,"result":result})),
        );
        Ok(())
    }
    #[handler]
    async fn observed(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<RestateJson<Option<Value>>, TerminalError> {
        Ok(RestateJson(
            ctx.get::<RestateJson<Value>>("observed")
                .await?
                .map(|j| j.0),
        ))
    }
}
pub async fn wait(runtime: &Runtime, command: &CreateCommand) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = runtime
                .harness
                .restate
                .invoke(
                    &Call::object(
                        "InvitationRequest",
                        &command.request_id.to_string(),
                        "observed",
                    ),
                    None::<&Value>,
                    None,
                )
                .await;
            assert_eq!(response.status, 200, "{response:?}");
            if !response.body.is_null() {
                break response.body;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}
