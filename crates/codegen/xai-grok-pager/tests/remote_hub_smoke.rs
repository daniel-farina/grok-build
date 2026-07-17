//! Smoke test for the multi-session remote hub (HTTP SPA backend).

use xai_grok_pager::remote::RemoteHubStart;

#[tokio::test]
async fn hub_registers_lists_publishes_and_disconnects() {
    let started = RemoteHubStart::start("127.0.0.1".into(), None, 0)
        .await
        .expect("start hub on ephemeral port");
    let handle = started.handle;
    assert!(handle.port > 0);

    let (tok_a, url_a) = handle
        .register_session("sess-a".into(), "Alpha".into())
        .await;
    let (tok_b, url_b) = handle
        .register_session("sess-b".into(), "Beta".into())
        .await;
    assert_ne!(tok_a, tok_b);
    assert!(url_a.contains(&tok_a) && url_b.contains(&tok_b));
    assert_eq!(handle.session_count().await, 2);

    // Re-register same session reuses token
    let (tok_a2, _) = handle
        .register_session("sess-a".into(), "Alpha".into())
        .await;
    assert_eq!(tok_a, tok_a2);
    assert_eq!(handle.session_count().await, 2);

    let slot = handle.get_by_session_id("sess-a").await.expect("slot a");
    slot.publish("user", "hi", "local");
    slot.publish("assistant_delta", " hello", "local");
    slot.publish("tool", "read foo.rs", "local");
    let hist = slot.history_snapshot();
    assert!(hist.iter().any(|e| e.kind == "user"));
    assert!(hist.iter().any(|e| e.kind == "tool"));

    // Permission payload event
    slot.publish_payload(
        "permission",
        "Allow shell?",
        "local",
        Some(serde_json::json!({
            "options": [
                {"option_id": "allow-once", "name": "Allow once"},
                {"option_id": "reject-once", "name": "Reject"}
            ]
        })),
    );
    assert!(slot
        .history_snapshot()
        .iter()
        .any(|e| e.kind == "permission"));

    assert!(handle.unregister_session("sess-a").await);
    assert_eq!(handle.session_count().await, 1);
    assert!(handle.unregister_session("sess-b").await);
    assert_eq!(handle.session_count().await, 0);
    handle.stop();
}


