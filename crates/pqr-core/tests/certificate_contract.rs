use pqr_core::certificate::{
    Certificate, CertificateError, Degraded, DegradedReason, Status, Target, TargetError,
};
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
    assert_eq!(value["schema"], "pqrguard/v0.1");
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

#[test]
fn verify_recomputes_claim_from_target() {
    let target = Target::new(10, 0.9, 0.01).unwrap();
    let cert = Certificate::certified("blake3:abc", 64, &target, &[16, 32, 64]);
    cert.verify().unwrap();
    let json = serde_json::to_string(&cert).unwrap();
    let parsed: Certificate = serde_json::from_str(&json).unwrap();
    parsed.verify().unwrap();

    let mut bad = parsed.clone();
    bad.claim.beta += 0.1;
    assert_eq!(bad.verify().unwrap_err(), CertificateError::ClaimBeta);
    bad = parsed.clone();
    bad.claim.m_allowed += 1;
    assert_eq!(bad.verify().unwrap_err(), CertificateError::ClaimMAllowed);
    bad = parsed.clone();
    bad.claim.expected_recall = 0.5;
    assert_eq!(
        bad.verify().unwrap_err(),
        CertificateError::ClaimExpectedRecall
    );
    bad = parsed.clone();
    bad.claim.theorem = "Thm2".to_string();
    assert_eq!(bad.verify().unwrap_err(), CertificateError::ClaimTheorem);
    bad = parsed.clone();
    bad.claim.kind = "recall".to_string();
    assert_eq!(bad.verify().unwrap_err(), CertificateError::ClaimKind);
    bad = parsed.clone();
    bad.claim.probability_space = "fixed_P".to_string();
    assert_eq!(
        bad.verify().unwrap_err(),
        CertificateError::ClaimProbabilitySpace
    );
}

#[test]
fn verify_rejects_invalid_structure_and_degraded_status() {
    let target = Target::new(10, 0.9, 0.01).unwrap();
    let cert = Certificate::certified("blake3:abc", 64, &target, &[16, 32, 64]);
    let mut bad = cert.clone();
    bad.status = Status::Degraded;
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Status);
    bad = cert.clone();
    bad.schema = "pqrguard/v0.2".to_string();
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Schema);
    bad = cert.clone();
    bad.dim = 2;
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Dimension);
    bad = cert.clone();
    bad.k = 0;
    assert_eq!(
        bad.verify().unwrap_err(),
        CertificateError::Target(TargetError::K)
    );
    bad = cert.clone();
    bad.stages.clear();
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Stages);
    bad = cert.clone();
    bad.stages.push(bad.stages[0].clone());
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Stages);
    bad = cert.clone();
    bad.assumptions.pop();
    assert_eq!(bad.verify().unwrap_err(), CertificateError::Assumptions);
    bad = cert.clone();
    bad.claim.beta = f64::NAN;
    assert_eq!(bad.verify().unwrap_err(), CertificateError::NonFinite);

    let mut value = serde_json::to_value(&cert).unwrap();
    value["recall"] = serde_json::json!(0.99);
    assert!(serde_json::from_value::<Certificate>(value).is_err());
}

#[test]
fn verify_accepts_one_ulp_claim_rounding() {
    let target = Target::new(10, 0.9, 0.01).unwrap();
    let cert = Certificate::certified("blake3:abc", 64, &target, &[16, 32, 64]);
    let mut rounded = cert.clone();
    rounded.claim.beta = f64::from_bits(cert.claim.beta.to_bits() + 1);
    rounded.claim.expected_recall = f64::from_bits(cert.claim.expected_recall.to_bits() + 1);
    rounded.verify().unwrap();
}

#[test]
fn verify_rejects_material_claim_delta() {
    let target = Target::new(10, 0.9, 0.01).unwrap();
    let cert = Certificate::certified("blake3:abc", 64, &target, &[16, 32, 64]);
    let mut forged = cert.clone();
    forged.claim.beta += 1e-6;
    assert_eq!(forged.verify().unwrap_err(), CertificateError::ClaimBeta);
    forged = cert.clone();
    forged.claim.expected_recall += 1e-6;
    assert_eq!(
        forged.verify().unwrap_err(),
        CertificateError::ClaimExpectedRecall
    );
}
