// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use super::super::types::ExceptionData;
use super::*;
use std::sync::Arc;
use temps_database::test_utils::TestDatabase;
use temps_entities::{error_events, error_groups, projects};

async fn setup_test_db() -> TestDatabase {
    TestDatabase::with_migrations()
        .await
        .expect("Failed to create test database")
}

async fn create_test_project(db: &Arc<DatabaseConnection>) -> i32 {
    use temps_entities::preset::Preset;
    use uuid::Uuid;

    let unique_slug = format!("test-project-{}", Uuid::new_v4());
    let project = projects::ActiveModel {
        name: Set("Test Project".to_string()),
        repo_name: Set("test-repo".to_string()),
        repo_owner: Set("test-owner".to_string()),
        directory: Set("/test".to_string()),
        main_branch: Set("main".to_string()),
        slug: Set(unique_slug),
        preset: Set(Preset::NextJs),
        created_at: Set(chrono::Utc::now()),
        updated_at: Set(chrono::Utc::now()),
        ..Default::default()
    };

    project
        .insert(db.as_ref())
        .await
        .expect("Failed to create project")
        .id
}

fn create_test_error_data(project_id: i32) -> CreateErrorEventData {
    CreateErrorEventData {
        source: Some("test".to_string()),
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Cannot read property 'foo' of undefined".to_string()),
        stack_trace: Some(serde_json::json!([
            {
                "filename": "/app/index.js",
                "function": "doSomething",
                "lineno": 42
            }
        ])),
        project_id,
        ..Default::default()
    }
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_creates_new_group() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;
    let error_data = create_test_error_data(project_id);

    let group_id = service
        .process_error_event(error_data)
        .await
        .expect("Failed to process error event");

    // Verify group was created
    let group = error_groups::Entity::find_by_id(group_id)
        .one(db.as_ref())
        .await
        .expect("Failed to fetch group")
        .expect("Group not found");

    assert_eq!(group.project_id, project_id);
    assert_eq!(group.total_count, 1);
    assert_eq!(group.status, "unresolved");
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_groups_similar_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;

    // Process first error
    let error_data1 = create_test_error_data(project_id);
    let group_id1 = service
        .process_error_event(error_data1.clone())
        .await
        .expect("Failed to process first error");

    // Process second identical error
    let error_data2 = error_data1.clone();
    let group_id2 = service
        .process_error_event(error_data2)
        .await
        .expect("Failed to process second error");

    // Should be grouped together
    assert_eq!(group_id1, group_id2);

    // Verify count was incremented
    let group = error_groups::Entity::find_by_id(group_id1)
        .one(db.as_ref())
        .await
        .expect("Failed to fetch group")
        .expect("Group not found");

    assert_eq!(group.total_count, 2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_generate_fingerprint_is_consistent() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let error_data = CreateErrorEventData {
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Test error".to_string()),
        stack_trace: Some(serde_json::json!([{"filename": "test.js", "function": "test"}])),
        project_id: 1,
        ..Default::default()
    };

    let fingerprint1 = service.generate_fingerprint(&error_data);
    let fingerprint2 = service.generate_fingerprint(&error_data);

    assert_eq!(fingerprint1, fingerprint2);
    assert!(!fingerprint1.is_empty());
}

#[tokio::test]
#[serial_test::serial]
async fn test_generate_fingerprint_differs_for_different_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let error_data1 = CreateErrorEventData {
        exception_type: Some("TypeError".to_string()),
        exception_value: Some("Error 1".to_string()),
        stack_trace: None,
        project_id: 1,
        ..Default::default()
    };

    let error_data2 = CreateErrorEventData {
        exception_type: Some("ReferenceError".to_string()),
        exception_value: Some("Error 2".to_string()),
        stack_trace: None,
        project_id: 1,
        ..Default::default()
    };

    let fingerprint1 = service.generate_fingerprint(&error_data1);
    let fingerprint2 = service.generate_fingerprint(&error_data2);

    assert_ne!(fingerprint1, fingerprint2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message1 = "Error: Connection failed at line 123";
    let message2 = "ERROR: CONNECTION FAILED AT LINE 123";

    let normalized1 = service.normalize_error_message(message1);
    let normalized2 = service.normalize_error_message(message2);

    assert_eq!(normalized1, normalized2);
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_creates_event() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());

    let project_id = create_test_project(&db).await;
    let error_data = create_test_error_data(project_id);

    let group_id = service
        .process_error_event(error_data)
        .await
        .expect("Failed to process error event");

    // Verify event was created
    let events = error_events::Entity::find()
        .filter(error_events::Column::ErrorGroupId.eq(group_id))
        .all(db.as_ref())
        .await
        .expect("Failed to fetch events");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].exception_type, "TypeError");
}

#[tokio::test]
#[serial_test::serial]
async fn test_extract_stack_signature() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let stack_trace = Some(serde_json::json!([
        {"filename": "/app/src/index.js", "function": "main"},
        {"filename": "/app/src/utils.js", "function": "helper"},
        {"filename": "/app/src/lib.js", "function": "doWork"},
    ]));

    let signature = service.extract_stack_signature(&stack_trace, 3);

    assert!(signature.contains("index.js"));
    assert!(signature.contains("main"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_uuids() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Resource 550e8400-e29b-41d4-a716-446655440000 not found";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<uuid>"));
    assert!(!normalized.contains("550e8400"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_hex_ids() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Transaction 0xdeadbeef1234 failed";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<hex_id>"));
    assert!(!normalized.contains("deadbeef"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_numbers() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: User 123456 failed to authenticate";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<num>"));
    assert!(!normalized.contains("123456"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_paths() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message1 = "Error: Cannot read /home/user/app/config.json";
    let normalized1 = service.normalize_error_message(message1);
    assert!(normalized1.contains("<path>"));

    let message2 = "Error: File C:\\Users\\Admin\\file.txt not found";
    let normalized2 = service.normalize_error_message(message2);
    assert!(normalized2.contains("<path>"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_urls() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Failed to fetch https://api.example.com/users/123";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<url>"));
    assert!(!normalized.contains("example.com"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_emails() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Email user@example.com already exists";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<email>"));
    assert!(!normalized.contains("user@example"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_ips() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Connection to 192.168.1.100 timeout";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("<ip>"));
    assert!(!normalized.contains("192.168"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_replaces_table_refs() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    let message = "Error: Foreign key constraint failed on table users_123";
    let normalized = service.normalize_error_message(message);

    assert!(normalized.contains("users_<id>"));
    assert!(!normalized.contains("users_123"));
}

#[tokio::test]
#[serial_test::serial]
async fn test_normalize_error_message_groups_similar_errors() {
    let test_db = setup_test_db().await;
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db);

    // These errors should normalize to the same message
    let message1 = "Error: User 12345 not found at 192.168.1.100";
    let message2 = "Error: User 67890 not found at 10.0.0.5";
    let message3 = "Error: User 99999 not found at 172.16.0.1";

    let normalized1 = service.normalize_error_message(message1);
    let normalized2 = service.normalize_error_message(message2);
    let normalized3 = service.normalize_error_message(message3);

    // All should normalize to the same pattern
    assert_eq!(normalized1, normalized2);
    assert_eq!(normalized2, normalized3);
    assert!(normalized1.contains("<num>"));
    assert!(normalized1.contains("<ip>"));
}

// ---------------------------------------------------------------------------
// Vector-similarity grouping (pgvector `<=>` cosine distance)
//
// pgvector's `<=>` operator returns `double precision` (FLOAT8). These tests
// run the real query against Postgres+pgvector so a mismatch between the SQL
// type and the Rust type the row is decoded into surfaces as a test failure
// instead of an HTTP 500 at ingest time.
// ---------------------------------------------------------------------------

/// Width of `error_groups.embedding` (`vector(384)`).
const EMBEDDING_DIMS: usize = 384;

/// Like `setup_test_db`, but returns `None` (so the test skips) when no
/// container runtime is reachable. Migration/schema/image failures still panic.
async fn try_setup_test_db() -> Option<TestDatabase> {
    match TestDatabase::with_migrations().await {
        Ok(test_db) => Some(test_db),
        Err(error) if temps_database::test_utils::is_container_runtime_unavailable(&error.to_string()) => {
            println!("Docker not available, skipping: {error}");
            None
        }
        Err(error) => panic!("Failed to create test database: {error}"),
    }
}

/// Build a 384-wide embedding that is zero everywhere except at the listed
/// `(index, value)` positions, so the cosine distance between two of them can
/// be worked out by hand.
fn sparse_embedding(entries: &[(usize, f32)]) -> error_groups::PgVector {
    let mut values = vec![0.0_f32; EMBEDDING_DIMS];
    for &(index, value) in entries {
        values[index] = value;
    }
    error_groups::PgVector(values)
}

/// Insert an `error_groups` row carrying an explicit embedding and status.
async fn insert_group_with_embedding(
    db: &Arc<DatabaseConnection>,
    project_id: i32,
    status: &str,
    embedding: error_groups::PgVector,
) -> i32 {
    let now = chrono::Utc::now();
    error_groups::ActiveModel {
        title: Set("TypeError: fixture".to_string()),
        error_type: Set("TypeError".to_string()),
        message_template: Set(Some("fixture".to_string())),
        embedding: Set(Some(embedding)),
        first_seen: Set(now),
        last_seen: Set(now),
        total_count: Set(1),
        status: Set(status.to_string()),
        assigned_to: Set(None),
        project_id: Set(project_id),
        environment_id: Set(None),
        deployment_id: Set(None),
        visitor_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db.as_ref())
    .await
    .expect("Failed to insert error group")
    .id
}

#[tokio::test]
#[serial_test::serial]
async fn test_find_similar_group_by_embedding_decodes_distance_of_matching_row() {
    let Some(test_db) = try_setup_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());
    let project_id = create_test_project(&db).await;

    let group_id =
        insert_group_with_embedding(&db, project_id, "unresolved", sparse_embedding(&[(0, 1.0)]))
            .await;

    // Cosine distance to the stored group is ~0.005, well inside the threshold,
    // so the query returns a row and its `distance` column must decode.
    let found = service
        .find_similar_group_by_embedding(&sparse_embedding(&[(0, 1.0), (1, 0.1)]), project_id)
        .await
        .expect("a matching row must decode instead of failing on the distance column");

    assert_eq!(found, Some(group_id));
}

#[tokio::test]
#[serial_test::serial]
async fn test_find_similar_group_by_embedding_returns_nearest_group() {
    let Some(test_db) = try_setup_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());
    let project_id = create_test_project(&db).await;

    // Both are inside the threshold; the farther one is inserted first so the
    // result cannot be explained by insertion order.
    let _farther = insert_group_with_embedding(
        &db,
        project_id,
        "unresolved",
        sparse_embedding(&[(0, 1.0), (1, 0.4)]),
    )
    .await;
    let nearer = insert_group_with_embedding(
        &db,
        project_id,
        "unresolved",
        sparse_embedding(&[(0, 1.0), (1, 0.1)]),
    )
    .await;

    let found = service
        .find_similar_group_by_embedding(&sparse_embedding(&[(0, 1.0), (1, 0.1)]), project_id)
        .await
        .expect("query must succeed");

    assert_eq!(found, Some(nearer));
}

#[tokio::test]
#[serial_test::serial]
async fn test_find_similar_group_by_embedding_respects_distance_threshold() {
    let Some(test_db) = try_setup_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());
    let query = sparse_embedding(&[(0, 1.0)]);

    // Orthogonal vector: cosine distance 1.0, far outside the 0.15 threshold.
    let orthogonal_project = create_test_project(&db).await;
    insert_group_with_embedding(
        &db,
        orthogonal_project,
        "unresolved",
        sparse_embedding(&[(7, 1.0)]),
    )
    .await;
    assert_eq!(
        service
            .find_similar_group_by_embedding(&query, orthogonal_project)
            .await
            .expect("query must succeed"),
        None
    );

    // (1.0, 0.65) is ~0.162 from (1.0, 0.0): just over the threshold.
    let just_over_project = create_test_project(&db).await;
    insert_group_with_embedding(
        &db,
        just_over_project,
        "unresolved",
        sparse_embedding(&[(0, 1.0), (1, 0.65)]),
    )
    .await;
    assert_eq!(
        service
            .find_similar_group_by_embedding(&query, just_over_project)
            .await
            .expect("query must succeed"),
        None
    );

    // (1.0, 0.55) is ~0.124 from (1.0, 0.0): just under the threshold.
    let just_under_project = create_test_project(&db).await;
    let just_under_group = insert_group_with_embedding(
        &db,
        just_under_project,
        "unresolved",
        sparse_embedding(&[(0, 1.0), (1, 0.55)]),
    )
    .await;
    assert_eq!(
        service
            .find_similar_group_by_embedding(&query, just_under_project)
            .await
            .expect("query must succeed"),
        Some(just_under_group)
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_find_similar_group_by_embedding_filters_by_status_and_project() {
    let Some(test_db) = try_setup_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());
    let project_id = create_test_project(&db).await;
    let other_project_id = create_test_project(&db).await;
    let embedding = sparse_embedding(&[(0, 1.0)]);

    // Identical embeddings, but resolved/ignored groups and other projects'
    // groups must never be returned.
    insert_group_with_embedding(&db, project_id, "resolved", embedding.clone()).await;
    insert_group_with_embedding(&db, project_id, "ignored", embedding.clone()).await;
    insert_group_with_embedding(&db, other_project_id, "unresolved", embedding.clone()).await;
    assert_eq!(
        service
            .find_similar_group_by_embedding(&embedding, project_id)
            .await
            .expect("query must succeed"),
        None
    );

    // An assigned group is still eligible.
    let assigned =
        insert_group_with_embedding(&db, project_id, "assigned", embedding.clone()).await;
    assert_eq!(
        service
            .find_similar_group_by_embedding(&embedding, project_id)
            .await
            .expect("query must succeed"),
        Some(assigned)
    );
}

#[tokio::test]
#[serial_test::serial]
async fn test_process_error_event_groups_similar_but_not_identical_errors() {
    let Some(test_db) = try_setup_test_db().await else {
        return;
    };
    let db = test_db.connection_arc();
    let service = ErrorIngestionService::new(db.clone());
    let project_id = create_test_project(&db).await;

    // Populate `exceptions`, as the Sentry ingest path does, so the text that is
    // embedded for the lookup matches the text stored on the created group.
    let error_with_message = |message: &str| CreateErrorEventData {
        exceptions: vec![ExceptionData {
            exception_type: "TypeError".to_string(),
            exception_value: Some(message.to_string()),
            stack_trace: create_test_error_data(project_id).stack_trace,
            mechanism: None,
            module: None,
            thread_id: None,
        }],
        ..create_test_error_data(project_id)
    };

    // Same message except for one short number: the fingerprint differs (the
    // normalizer only masks numbers of 4+ digits) but the embeddings are
    // nearly identical, so the second event must take the similarity path.
    let first = error_with_message("Failed to load item 42 from the upstream inventory service");
    let second = error_with_message("Failed to load item 43 from the upstream inventory service");
    assert_ne!(
        service.generate_fingerprint(&first),
        service.generate_fingerprint(&second),
        "fixture must miss the exact-fingerprint fast path"
    );

    let first_group = service
        .process_error_event(first)
        .await
        .expect("first error must be ingested");
    let second_group = service
        .process_error_event(second)
        .await
        .expect("a similar-but-not-identical error must be ingested, not fail with a decode error");

    assert_eq!(first_group, second_group);

    let group = error_groups::Entity::find_by_id(first_group)
        .one(db.as_ref())
        .await
        .expect("Failed to fetch group")
        .expect("Group not found");
    assert_eq!(group.total_count, 2);

    let events = error_events::Entity::find()
        .filter(error_events::Column::ErrorGroupId.eq(first_group))
        .all(db.as_ref())
        .await
        .expect("Failed to fetch events");
    assert_eq!(events.len(), 2);

    // A genuinely different error still opens its own group.
    let unrelated = error_with_message("Connection refused while opening the payment gateway socket");
    let unrelated_group = service
        .process_error_event(unrelated)
        .await
        .expect("unrelated error must be ingested");
    assert_ne!(unrelated_group, first_group);
}
