use ghinvite_core::{
    RequestId,
    storage::{SigninBudgetStorage, signin_budget::SigninBudgetKey},
};
use ghinvite_storage_sqlx::SqlxStorage;

#[tokio::test]
async fn independent_connections_atomically_admit_only_five_starts() {
    let path = std::env::temp_dir().join(format!("signin-budget-{}.sqlite", RequestId::new()));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    let key = SigninBudgetKey::from_digest([3; 32]);
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..50 {
        let storage = storage.clone();
        tasks.spawn(async move {
            storage
                .reserve_signin(key, RequestId::new(), 1_000_000)
                .await
                .unwrap()
        });
    }
    let mut allowed = 0;
    while let Some(result) = tasks.join_next().await {
        allowed += usize::from(result.unwrap());
    }
    assert_eq!(allowed, 5);
    assert!(
        !storage
            .reserve_signin(key, RequestId::new(), 1_599_999)
            .await
            .unwrap()
    );
    assert!(
        storage
            .reserve_signin(key, RequestId::new(), 1_600_000)
            .await
            .unwrap()
    );
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
