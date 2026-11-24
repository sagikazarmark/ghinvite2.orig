use ghinvite_core::storage::test_suite::scenario_email_domains;
use ghinvite_storage_sqlx::SqlxStorage;

#[tokio::test]
async fn raw_domain_guards_preserve_rules_and_evidence() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-domains-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    scenario_email_domains(storage.clone()).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    let links: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,email_domains_json FROM invitation_links ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let requests: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id,domain_evidence_json FROM invitation_requests ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let restricted = &links
        .iter()
        .find(|r| r.1.as_deref() != Some("[]"))
        .unwrap()
        .0;
    let evidenced = &requests.iter().find(|r| r.1.is_some()).unwrap().0;
    let insert_link = "INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,projection_revision,projection_content,secret_generation,secret_sha256,email_domains_json) SELECT 'domain-raw-control',installation_id,account_id,created_by,created_at,approval_required,projection_revision,projection_content,secret_generation,secret_sha256,? FROM invitation_links WHERE id=?";
    let valid_rules = links
        .iter()
        .find(|r| &r.0 == restricted)
        .unwrap()
        .1
        .as_ref()
        .unwrap();
    sqlx::query(insert_link)
        .bind(valid_rules)
        .bind(restricted)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM invitation_links WHERE id='domain-raw-control'")
        .execute(&pool)
        .await
        .unwrap();
    let mut malformed_rules = vec![
        "null".to_owned(),
        "{}".into(),
        "not json".into(),
        "[{}]".into(),
        "[{\"exact\":\"a@example.com\"}]".into(),
        "[{\"exact\":\"UPPER.EXAMPLE\"}]".into(),
        "[{\"exact\":\"a.example\"},{\"exact\":\"a.example\"}]".into(),
    ];
    malformed_rules.push(
        serde_json::to_string(
            &(0..33)
                .map(|n| serde_json::json!({"exact":format!("n{n}.example")}))
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    );
    for value in malformed_rules {
        assert!(
            sqlx::query(insert_link)
                .bind(value)
                .bind(restricted)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    let insert_request = "INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision,domain_evidence_json) SELECT 'domain-proof-control',invitation_link_id,requester_id,'pending',created_at,1,? FROM invitation_requests WHERE id=?";
    let proof = requests
        .iter()
        .find(|r| &r.0 == evidenced)
        .unwrap()
        .1
        .as_ref()
        .unwrap();
    sqlx::query(insert_request)
        .bind(proof)
        .bind(evidenced)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM invitation_requests WHERE id='domain-proof-control'")
        .execute(&pool)
        .await
        .unwrap();
    let mut malformed_proofs = vec![
        None,
        Some("null".to_owned()),
        Some("{}".into()),
        Some("not json".into()),
    ];
    for domain in [
        "evil.example",
        "xn--bcher-kva.example",
        "users.noreply.github.com",
        "a@example.com",
    ] {
        let mut value: serde_json::Value = serde_json::from_str(proof).unwrap();
        value["matched_domains"] = serde_json::json!([domain]);
        malformed_proofs.push(Some(value.to_string()));
    }
    for value in malformed_proofs {
        assert!(
            sqlx::query(insert_request)
                .bind(value)
                .bind(evidenced)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    let replace_link = insert_link
        .replace("INSERT INTO", "INSERT OR REPLACE INTO")
        .replace("'domain-raw-control'", "?");
    assert!(
        sqlx::query(sqlx::AssertSqlSafe(replace_link))
            .bind(restricted)
            .bind("[{\"exact\":\"changed.example\"}]")
            .bind(restricted)
            .execute(&pool)
            .await
            .is_err()
    );
    let replace_request = insert_request
        .replace("INSERT INTO", "INSERT OR REPLACE INTO")
        .replace("'domain-proof-control'", "?");
    let mut changed_proof: serde_json::Value = serde_json::from_str(proof).unwrap();
    changed_proof["matched_domains"] = serde_json::json!(["example.com"]);
    assert!(
        sqlx::query(sqlx::AssertSqlSafe(replace_request))
            .bind(evidenced)
            .bind(changed_proof.to_string())
            .bind(evidenced)
            .execute(&pool)
            .await
            .is_err()
    );
    for value in [
        "null",
        "{}",
        "not json",
        "[]",
        "[{\"exact\":\"evil.example\"}]",
        "[{\"exact\":\"UPPER.EXAMPLE\"}]",
    ] {
        assert!(
            sqlx::query("UPDATE invitation_links SET email_domains_json=? WHERE id=?")
                .bind(value)
                .bind(restricted)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    for value in [None, Some("null"), Some("{}"), Some("not json")] {
        assert!(
            sqlx::query("UPDATE invitation_requests SET domain_evidence_json=? WHERE id=?")
                .bind(value)
                .bind(evidenced)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert!(sqlx::query("UPDATE invitation_requests SET domain_evidence_json=json_set(domain_evidence_json,'$.matched_domains[0]','evil.example') WHERE id=?").bind(evidenced).execute(&pool).await.is_err());
    assert_eq!(
        links,
        sqlx::query_as("SELECT id,email_domains_json FROM invitation_links ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap()
    );
    assert_eq!(
        requests,
        sqlx::query_as("SELECT id,domain_evidence_json FROM invitation_requests ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap()
    );
    pool.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
