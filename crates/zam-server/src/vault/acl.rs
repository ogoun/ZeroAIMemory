//! Windows: DACL файла мастер-ключа — только владелец (`OWNER RIGHTS`), SYSTEM и
//! администраторы, без наследования от каталога (аналог 0600 на Linux); проверка DACL
//! существующего файла ([`foreign_readers`], аналог `mode & 0o077` на Linux).
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

/// Защищённый DACL: полный доступ владельцу, SYSTEM и BUILTIN\Administrators.
pub const KEY_FILE_SDDL: &str = "D:P(A;;FA;;;OW)(A;;FA;;;SY)(A;;FA;;;BA)";

/// SID, которым чтение ключа разрешено и при [`KEY_FILE_SDDL`]: SYSTEM, BUILTIN\Administrators,
/// OWNER RIGHTS (владелец файла сравнивается отдельно).
const TRUSTED_SIDS: [&str; 3] = ["S-1-5-18", "S-1-5-32-544", "S-1-3-4"];

/// Права, дающие чтение содержимого: `FILE_READ_DATA`, `GENERIC_READ`, `GENERIC_ALL`.
const READ_MASK: u32 = 0x0000_0001 | 0x8000_0000 | 0x1000_0000;
/// `ACCESS_ALLOWED_ACE_TYPE`.
const ALLOWED_ACE: u8 = 0;
/// `INHERIT_ONLY_ACE`: запись только для наследников, к самому файлу не применяется.
const INHERIT_ONLY: u8 = 0x08;

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Ставит файлу `path` DACL [`KEY_FILE_SDDL`] (наследование от каталога отключено).
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    set_dacl(path, KEY_FILE_SDDL)
}

/// SID (строкой), кроме владельца файла, SYSTEM, администраторов и `OWNER RIGHTS`, которым
/// DACL файла `path` разрешает чтение содержимого (`NULL DACL` — `Everyone`). Учитываются
/// разрешающие записи, применимые к самому файлу; запрещающие только сужают доступ, поэтому
/// ответ — верхняя оценка (предупреждение может быть лишним, пропуска нет).
pub fn foreign_readers(path: &Path) -> io::Result<Vec<String>> {
    let name = wide(path.as_os_str());
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: строка завершена нулём; указатели владельца и DACL указывают внутрь
    // дескриптора, выделенного системой, и используются до LocalFree в конце; записи ACL
    // читаются через GetAce в пределах AceCount.
    unsafe {
        let e = GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        );
        if e.is_err() {
            return Err(io::Error::from_raw_os_error(e.0 as i32));
        }
        let r = (|| {
            if dacl.is_null() {
                return Ok(vec!["Everyone (NULL DACL)".to_string()]);
            }
            let owner = sid_string(owner)?;
            let mut out = Vec::new();
            for i in 0..u32::from((*dacl).AceCount) {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, i, &mut ace).map_err(io::Error::other)?;
                let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
                if ace.Header.AceType != ALLOWED_ACE
                    || ace.Header.AceFlags & INHERIT_ONLY != 0
                    || ace.Mask & READ_MASK == 0
                {
                    continue;
                }
                let sid = sid_string(PSID(&ace.SidStart as *const u32 as *mut _))?;
                if sid != owner && !TRUSTED_SIDS.contains(&sid.as_str()) && !out.contains(&sid) {
                    out.push(sid);
                }
            }
            Ok(out)
        })();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// SID строкой (`S-1-…`).
///
/// # Safety
/// `sid` — действительный SID.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut out = PWSTR::null();
    // SAFETY: по контракту функции; строка, выделенная системой, освобождается LocalFree
    unsafe {
        ConvertSidToStringSidW(sid, &mut out).map_err(io::Error::other)?;
        let s = out.to_string().map_err(io::Error::other);
        let _ = LocalFree(Some(HLOCAL(out.0 as _)));
        s
    }
}

/// [`set_dacl`] для тестов других модулей.
#[cfg(test)]
pub fn set_dacl_for_test(path: &Path, sddl: &str) {
    set_dacl(path, sddl).unwrap();
}

/// Ставит файлу `path` защищённый DACL из SDDL `sddl`.
fn set_dacl(path: &Path, sddl: &str) -> io::Result<()> {
    let sddl = wide(sddl);
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
    use windows::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;

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
        assert_eq!(foreign_readers(&p).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn foreign_readers_found_in_dacl() {
        let dir = crate::test_util::dir_in_target("vault-acl-readers");
        let p = dir.join("k");
        std::fs::write(&p, "x").unwrap();
        // чтение «Все» и «Пользователи», запись только для наследников — не применяется
        set_dacl(
            &p,
            "D:P(A;;FA;;;OW)(A;;FR;;;WD)(A;;FR;;;BU)(A;OIIO;FA;;;AU)(A;;0x100000;;;IU)",
        )
        .unwrap();
        assert_eq!(foreign_readers(&p).unwrap(), ["S-1-1-0", "S-1-5-32-545"]);
        restrict_to_owner(&p).unwrap();
        assert!(foreign_readers(&p).unwrap().is_empty());
    }
}
