//! Сканер секретов вынесен в крейт `zam-secrets` (задача 15b M6): `zam_markdown::secrets` и
//! прежние имена (`zam_markdown::mask_text`, …) — реэкспорт того же кода, поведение не меняется.
//! Корпус — общий с тестами сканера (`crates/zam-secrets/tests/common`).

#[path = "../../zam-secrets/tests/common/mod.rs"]
mod common;

/// `zam_markdown::secrets::mask_text` ≡ `zam_secrets::mask_text` (и прежние имена) на корпусе
/// тестов сканера, склейках атомов генератора и закреплённом входе упора.
#[test]
fn secrets_reexport_identical() {
    assert_eq!(zam_markdown::SECRET_MARK, zam_secrets::SECRET_MARK);
    assert_eq!(zam_markdown::secrets::SECRET_MARK, common::MARK);
    let atoms = common::atoms();
    let mut inputs = common::corpus();
    inputs.extend(
        (0..2000u64).map(|s| common::expand(&common::combo(&atoms, s, 1 + (s % 14) as usize))),
    );
    inputs.push(common::expand(
        include_str!("../../zam-secrets/tests/fixtures/scan_depth_capped.txt").trim_end(),
    ));
    let mut masked = 0;
    for t in &inputs {
        let m = zam_secrets::mask_text(t);
        assert_eq!(zam_markdown::secrets::mask_text(t), m, "{t:?}");
        assert_eq!(zam_markdown::mask_text(t), m, "{t:?}");
        let hits = zam_secrets::scan_secrets(t);
        assert_eq!(zam_markdown::scan_secrets(t), hits, "{t:?}");
        assert_eq!(zam_markdown::mask_secrets(t, &hits), m, "{t:?}");
        assert_eq!(
            zam_markdown::scan_secrets_ext(t),
            zam_secrets::scan_secrets_ext(t),
            "{t:?}"
        );
        masked += usize::from(m != *t);
    }
    assert!(masked > 100, "корпус без секретов: {masked}");
}
