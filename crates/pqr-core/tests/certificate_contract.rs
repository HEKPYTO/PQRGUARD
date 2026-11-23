use pqr_core::certificate::{Certificate, Degraded, DegradedReason, Status, Target, TargetError};
use pqr_core::math::MathError;

#[test]
fn target_rejects_unsupported_config() {
    assert_eq!(Target::new(0, 1.0, 0.01).unwrap_err(), TargetError::K);
    assert_eq!(Target::new(10, 0.0, 0.01).unwrap_err(), TargetError::Tau);
    assert_eq!(Target::new(10, 1.1, 0.01).unwrap_err(), TargetError::Tau);
    assert_eq!(Target::new(10, 1.0, 0.0).unwrap_err(), TargetError::Delta);
    assert_eq!(Target::new(10, 1.0, 1.0).unwrap_err(), TargetError::Delta);
    assert!(matches!(
        Target::new(1, 1.0, 0.9),
        Err(TargetError::Math(MathError::Budget))
    ));
}

#[test]
fn certificate_carries_theorem_fields_and_no_recall_key() {
    let target = Target::new(10, 0.9, 0.01).unwrap();
    let cert = Certificate::certified("blake3:abc", 64, &target, &[128, 256, 512, 640]);
    let value = serde_json::to_value(&cert).unwrap();
    assert_eq!(value["schema"], "pqr.guard/v0.1");
    assert_eq!(value["claim"]["theorem"], "Thm3");
    assert!((value["claim"]["beta"].as_f64().unwrap() - 0.001).abs() < 1e-15);
    assert_eq!(value["claim"]["m_allowed"], 0);
    assert_eq!(value["status"], "certified");
    assert!(value.get("recall").is_none());
    assert!(value["claim"].get("recall").is_none());
}

#[test]
fn degraded_never_upgrades() {
    let target = Target::new(1, 1.0, 0.01).unwrap();
    let degraded = Degraded::new(
        "blake3:abc",
        64,
        &target,
        vec![DegradedReason::AdaptiveSequence],
    );
    let value = serde_json::to_value(&degraded).unwrap();
    assert_eq!(value["status"], "degraded");
    assert!(value.get("claim").is_none());
    assert_eq!(degraded.status, Status::Degraded);
}
