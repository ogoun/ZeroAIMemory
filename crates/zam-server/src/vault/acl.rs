//! Windows: DACL файла мастер-ключа — только владелец (`OWNER RIGHTS`), SYSTEM и
//! администраторы, без наследования от каталога (аналог 0600 на Linux).
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows::core::{BOOL, PCWSTR};

/// Защищённый DACL: полный доступ владельцу, SYSTEM и BUILTIN\Administrators.
pub const KEY_FILE_SDDL: &str = "D:P(A;;FA;;;OW)(A;;FA;;;SY)(A;;FA;;;BA)";

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Ставит файлу `path` DACL [`KEY_FILE_SDDL`] (наследование от каталога отключено).
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let sddl = wide(KEY_FILE_SDDL);
    let name = wide(path.as_os_str());
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: строки завершены нулём и живут до конца функции; дескриптор, выделенный
    // системой, освобождается LocalFree; указатель DACL указывает внутрь дескриптора.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(io::Error::other)?;
        let mut present = BOOL(0);
        let mut defaulted = BOOL(0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let r = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted)
            .map_err(io::Error::other)
            .and_then(|()| {
                let e = SetNamedSecurityInfoW(
                    PCWSTR(name.as_ptr()),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                    None,
                    Some(dacl as *const ACL),
                    None,
                );
                if e.is_ok() {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(e.0 as i32))
                }
            });
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
    };
    use windows::core::PWSTR;

    /// SDDL DACL файла.
    fn dacl_sddl(path: &Path) -> String {
        let name = wide(path.as_os_str());
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let mut out = PWSTR::null();
        unsafe {
            let e = GetNamedSecurityInfoW(
                PCWSTR(name.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut sd,
            );
            assert!(e.is_ok(), "GetNamedSecurityInfoW: {e:?}");
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                sd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut out,
                None,
            )
            .unwrap();
            let s = out.to_string().unwrap();
            let _ = LocalFree(Some(HLOCAL(out.0 as _)));
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            s
        }
    }

    #[test]
    fn key_file_dacl_protected_owner_only() {
        let dir = crate::test_util::dir_in_target("vault-acl");
        let p = dir.join("k");
        std::fs::write(&p, "x").unwrap();
        restrict_to_owner(&p).unwrap();
        let s = dacl_sddl(&p);
        // защищённый (без наследуемых записей), только OW/SY/BA
        assert!(s.starts_with("D:P"), "{s}");
        assert!(!s.contains("ID;"), "унаследованные записи: {s}");
        for sid in ["WD", "BU", "AU"] {
            assert!(!s.contains(&format!(";;;{sid})")), "{sid} в {s}");
        }
        // владелец по-прежнему читает
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x");
    }
}
