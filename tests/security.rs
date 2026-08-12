use eupho::security::{
    RevisionLedger, SignedEnvelope, canonical_digest, canonical_json, envelope_payload_digest,
    sign_envelope, verify_envelope,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tempfile::tempdir;

#[test]
fn canonical_json_uses_rfc8785_key_and_number_rules() {
    let value = json!({"z":[{"y":2,"x":1},"last"],"a":{"beta":true,"alpha":null},"n":1.0});
    assert_eq!(
        canonical_json(&value).unwrap(),
        r#"{"a":{"alpha":null,"beta":true},"n":1,"z":[{"x":1,"y":2},"last"]}"#
    );
    assert_eq!(
        canonical_digest(&value).unwrap(),
        canonical_digest(&json!({"n":1,"a":{"alpha":null,"beta":true},"z":[{"x":1,"y":2},"last"]}))
            .unwrap()
    );
}

#[test]
fn signed_metadata_is_order_stable_and_rejects_tampering() {
    let key = b"test-only-signing-secret";
    let envelope = sign_envelope(&json!({"z":3,"nested":{"b":2,"a":1}}), "active", key).unwrap();
    let keys = HashMap::from([("active".to_owned(), key.to_vec())]);
    assert_eq!(verify_envelope(&envelope, &keys).unwrap(), envelope.payload);
    assert_eq!(
        envelope.mac,
        sign_envelope(&json!({"nested":{"a":1,"b":2},"z":3}), "active", key)
            .unwrap()
            .mac
    );

    let tampered = SignedEnvelope {
        payload: json!({"z":4,"nested":{"b":2,"a":1}}),
        ..envelope
    };
    assert_eq!(
        verify_envelope(&tampered, &keys).unwrap_err().code,
        "invalid_signature"
    );
}

#[test]
fn signatures_reject_unknown_keys_and_malformed_macs() {
    let envelope =
        sign_envelope(&json!({"runId":"run-7","revision":1}), "retired", b"secret").unwrap();
    assert_eq!(
        verify_envelope(&envelope, &HashMap::new())
            .unwrap_err()
            .code,
        "unknown_signing_key"
    );
    let malformed = SignedEnvelope {
        mac: "not-hex".to_owned(),
        ..envelope
    };
    let keys = HashMap::from([("retired".to_owned(), b"secret".to_vec())]);
    assert_eq!(
        verify_envelope(&malformed, &keys).unwrap_err().code,
        "invalid_signature"
    );
}

#[test]
fn revision_ledger_rejects_rollbacks_forks_and_key_rewrites() {
    let state = tempdir().unwrap();
    let ledger = RevisionLedger::new(state.path());
    let digest_one = envelope_payload_digest(&json!({"revision":1,"status":"ready"})).unwrap();

    assert_eq!(ledger.read("run-123").unwrap(), None);
    ledger.prepare("run-123", 1, &digest_one, "active").unwrap();
    let prepared = ledger.read("run-123").unwrap().unwrap();
    assert_eq!(prepared.revision, 1);
    assert!(!prepared.confirmed);
    assert_eq!(prepared.payload_digest, digest_one);

    assert_eq!(
        ledger
            .assert_fresh("run-123", 0, &digest_one)
            .unwrap_err()
            .code,
        "invalid_revision"
    );
    assert_eq!(
        ledger
            .prepare("run-123", 1, &digest_one, "different-key")
            .unwrap_err()
            .code,
        "metadata_fork"
    );
    let fork = envelope_payload_digest(&json!({"revision":1,"status":"changed"})).unwrap();
    assert_eq!(
        ledger.assert_fresh("run-123", 1, &fork).unwrap_err().code,
        "metadata_fork"
    );
    assert_eq!(
        ledger.confirm("run-123", 2, &digest_one).unwrap_err().code,
        "revision_confirmation_mismatch"
    );
    ledger.confirm("run-123", 1, &digest_one).unwrap();
    assert!(ledger.read("run-123").unwrap().unwrap().confirmed);

    let digest_two = envelope_payload_digest(&json!({"revision":2,"status":"claimed"})).unwrap();
    ledger.prepare("run-123", 2, &digest_two, "next").unwrap();
    assert_eq!(
        ledger
            .assert_fresh("run-123", 1, &digest_one)
            .unwrap_err()
            .code,
        "metadata_rollback"
    );
}

#[test]
fn revision_ledger_rejects_unsafe_run_ids_and_malformed_anchors() {
    let state = tempdir().unwrap();
    let ledger = RevisionLedger::new(state.path());
    assert_eq!(
        ledger.read("../outside").unwrap_err().code,
        "invalid_run_id"
    );
    assert_eq!(
        ledger
            .prepare("valid-run", 0, &"0".repeat(64), "active")
            .unwrap_err()
            .code,
        "invalid_revision"
    );

    std::fs::create_dir_all(state.path().join("revisions")).unwrap();
    std::fs::write(
        state.path().join("revisions/run-corrupt.json"),
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "runId": "run-corrupt",
            "revision": 1,
            "payloadDigest": "0".repeat(64),
            "keyId": "active",
            "confirmed": false
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        ledger.read("run-corrupt").unwrap_err().code,
        "invalid_revision_anchor"
    );
}

#[test]
fn payload_digest_ignores_object_insertion_order() {
    let left: Value =
        serde_json::from_str(r#"{"repository":"acme/widgets","revision":2}"#).unwrap();
    let right: Value =
        serde_json::from_str(r#"{"revision":2,"repository":"acme/widgets"}"#).unwrap();
    assert_eq!(
        envelope_payload_digest(&left).unwrap(),
        envelope_payload_digest(&right).unwrap()
    );
}
