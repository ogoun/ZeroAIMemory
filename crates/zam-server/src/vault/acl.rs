//! Windows: DACL файла мастер-ключа — только владелец (`OWNER RIGHTS`), SYSTEM и
//! администраторы, без наследования от каталога (аналог 0600 на Linux); проверка DACL
//! существующего файла ([`foreign_readers`], аналог `mode & 0o077` на Linux).
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetSecurityDescriptorDacl,
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_MODE,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{BOOL, PCWSTR, PWSTR};

/// Защищённый DACL: полный доступ владельцу, SYSTEM и BUILTIN\Administrators.
pub const KEY_FILE_SDDL: &str = "D:P(A;;FA;;;OW)(A;;FA;;;SY)(A;;FA;;;BA)";

/// SYSTEM.
const SID_SYSTEM: &str = "S-1-5-18";
/// BUILTIN\Administrators.
const SID_ADMINS: &str = "S-1-5-32-544";
/// `OWNER RIGHTS`: права владельца файла, кто бы им ни был.
const SID_OWNER_RIGHTS: &str = "S-1-3-4";

/// Права, дающие чтение содержимого: `FILE_READ_DATA`, `GENERIC_READ`, `GENERIC_ALL`,
/// `MAXIMUM_ALLOWED`.
const READ_MASK: u32 = 0x0000_0001 | 0x8000_0000 | 0x1000_0000 | 0x0200_0000;
/// Разрешающие типы ACE: `ACCESS_ALLOWED` (0), `ACCESS_ALLOWED_OBJECT` (5),
/// `ACCESS_ALLOWED_CALLBACK` (9), `ACCESS_ALLOWED_CALLBACK_OBJECT` (11).
const ALLOWED_TYPES: [u8; 4] = [0, 5, 9, 11];
/// Объектные типы (5, 11): после маски — флаги и необязательные GUID, затем SID.
const OBJECT_TYPES: [u8; 2] = [5, 11];
/// `INHERIT_ONLY_ACE`: запись только для наследников, к самому файлу не применяется.
const INHERIT_ONLY: u8 = 0x08;

fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Ставит файлу `path` DACL [`KEY_FILE_SDDL`] (наследование от каталога отключено).
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    set_dacl(path, KEY_FILE_SDDL)
}

/// Создаёт новый файл (`CREATE_NEW`, занятое имя — `AlreadyExists`) для записи **сразу** с
/// защищённым DACL [`KEY_FILE_SDDL`]: дескриптор безопасности передаётся в `CreateFileW`
/// (`SECURITY_ATTRIBUTES`), поэтому файла с унаследованными правами каталога не бывает ни
/// мгновения — аналог `mode(0o600)` при создании на Linux. Совместный доступ запрещён
/// (`share mode = 0`), пока дескриптор открыт.
pub fn create_private_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::windows::io::FromRawHandle;
    let sddl = wide(KEY_FILE_SDDL);
    let name = wide(path.as_os_str());
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: строки завершены нулём и живут до конца функции; дескриптор безопасности,
    // выделенный системой, нужен только на время CreateFileW и освобождается LocalFree;
    // полученный HANDLE принадлежит только возвращаемому File.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .map_err(io::Error::other)?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0,
            bInheritHandle: BOOL(0),
        };
        let h = CreateFileW(
            PCWSTR(name.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            Some(&sa),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        );
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        let h = h.map_err(win32_io)?;
        Ok(std::fs::File::from_raw_handle(h.0))
    }
}

/// Ошибка Win32 (`HRESULT` 0x8007xxxx) — `io::Error` с кодом Win32 (`ErrorKind` по нему:
/// `ERROR_FILE_EXISTS` → `AlreadyExists`); прочие — как есть.
fn win32_io(e: windows::core::Error) -> io::Error {
    let c = e.code().0 as u32;
    if c & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((c & 0xFFFF) as i32)
    } else {
        io::Error::other(e)
    }
}

/// SID (строкой), которым DACL файла `path` разрешает чтение содержимого, кроме доверенных
/// (`NULL DACL` — `Everyone`). Учитываются все разрешающие типы ACE (обычные, объектные,
/// с обратным вызовом), применимые к самому файлу; запрещающие только сужают доступ и не
/// вычитаются, поэтому ответ — верхняя оценка (лишнее предупреждение возможно, пропуска нет).
///
/// Доверенные **[решение исполнителя]**: учётная запись процесса сервера, SYSTEM и
/// администраторы. Владелец файла и `OWNER RIGHTS` доверенные, только если владелец — сам
/// сервер или администраторы; иначе (ключ восстановлен из бэкапа другим пользователем и тот
/// остался владельцем) владелец читает ключ как чужой и попадает в ответ — см.
/// [`untrusted_readers`].
pub fn foreign_readers(path: &Path) -> io::Result<Vec<String>> {
    let me = process_user_sid()?;
    let name = wide(path.as_os_str());
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: строка завершена нулём; указатели владельца и DACL указывают внутрь
    // дескриптора, выделенного системой, и используются до LocalFree в конце; записи ACL
    // читаются через GetAce в пределах AceCount, SID — по смещению внутри записи (AceSize).
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
            let mut readers = Vec::new();
            for i in 0..u32::from((*dacl).AceCount) {
                let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                GetAce(dacl, i, &mut ace).map_err(io::Error::other)?;
                let base = ace as *const u8;
                let h = &*(base as *const ACE_HEADER);
                if !ALLOWED_TYPES.contains(&h.AceType) || h.AceFlags & INHERIT_ONLY != 0 {
                    continue;
                }
                // заголовок (4), маска (4); у объектных — флаги (4) и GUID по флагам
                let mask = (base.add(4) as *const u32).read_unaligned();
                if mask & READ_MASK == 0 {
                    continue;
                }
                let mut off = 8usize;
                if OBJECT_TYPES.contains(&h.AceType) {
                    let flags = (base.add(8) as *const u32).read_unaligned();
                    off += 4 + 16 * usize::from(flags & 1 != 0) + 16 * usize::from(flags & 2 != 0);
                }
                if off >= usize::from(h.AceSize) {
                    continue;
                }
                readers.push(sid_string(PSID(base.add(off) as *mut _))?);
            }
            Ok(untrusted_readers(&owner, &me, readers))
        })();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        r
    }
}

/// Отбор недоверенных из SID, которым DACL даёт чтение (порядок первого появления, без
/// повторов). Доверены `me` (процесс сервера), SYSTEM и администраторы; `owner` и
/// `OWNER RIGHTS` — только если владелец сам доверен. Недоверенный владелец, получающий доступ
/// через `OWNER RIGHTS`, указывается своим SID.
fn untrusted_readers(owner: &str, me: &str, readers: Vec<String>) -> Vec<String> {
    let trusted = |sid: &str| sid == me || sid == SID_SYSTEM || sid == SID_ADMINS;
    let owner_trusted = trusted(owner);
    let mut out: Vec<String> = Vec::new();
    for sid in readers {
        let sid = if sid == SID_OWNER_RIGHTS {
            if owner_trusted {
                continue;
            }
            owner.to_string()
        } else {
            sid
        };
        if !trusted(&sid) && !out.contains(&sid) {
            out.push(sid);
        }
    }
    out
}

/// SID учётной записи процесса (`TokenUser` токена процесса).
fn process_user_sid() -> io::Result<String> {
    // SAFETY: токен процесса открывается на чтение и закрывается; буфер `u64` выровнен под
    // `TOKEN_USER`, его размер — из первого вызова GetTokenInformation; SID указывает внутрь
    // буфера и читается до его освобождения.
    unsafe {
        let mut tok = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok).map_err(io::Error::other)?;
        let r = (|| {
            let mut len = 0u32;
            let _ = GetTokenInformation(tok, TokenUser, None, 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            GetTokenInformation(
                tok,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                (buf.len() * 8) as u32,
                &mut len,
            )
            .map_err(io::Error::other)?;
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            sid_string(tu.User.Sid)
        })();
        let _ = CloseHandle(tok);
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

    /// Файл создаётся сразу с защищённым DACL (ревью финальной волны, M-1): пока дескриптор
    /// открыт и файл пуст, DACL уже `D:P` без унаследованных записей, а чужое открытие
    /// невозможно (`share mode = 0`); занятое имя — `AlreadyExists`.
    #[test]
    fn private_file_created_with_protected_dacl() {
        use std::io::Write;
        let dir = crate::test_util::dir_in_target("vault-acl-create");
        let p = dir.join("k.tmp");
        let mut f = create_private_file(&p).unwrap();
        let s = dacl_sddl(&p);
        assert!(s.starts_with("D:P") && !s.contains("ID;"), "{s}");
        assert!(foreign_readers(&p).unwrap().is_empty());
        assert!(
            std::fs::File::open(&p).is_err(),
            "чужое открытие при открытом дескрипторе"
        );
        f.write_all(b"key").unwrap();
        f.sync_all().unwrap();
        drop(f);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "key");
        assert_eq!(
            create_private_file(&p).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    /// Владелец, который не процесс сервера, — чужой читатель (ключ из бэкапа под другим
    /// пользователем); доверенный владелец и `OWNER RIGHTS` при нём — нет (M-2).
    #[test]
    fn untrusted_owner_is_reported() {
        let me = "S-1-5-21-1-1001";
        let other = "S-1-5-21-1-1002";
        let r = |owner: &str, v: &[&str]| {
            untrusted_readers(owner, me, v.iter().map(|s| s.to_string()).collect())
        };
        assert!(r(me, &[SID_OWNER_RIGHTS, me, SID_SYSTEM, SID_ADMINS]).is_empty());
        assert!(
            r(SID_ADMINS, &[SID_OWNER_RIGHTS]).is_empty(),
            "владелец — администраторы"
        );
        assert_eq!(r(other, &[SID_OWNER_RIGHTS, SID_SYSTEM]), [other]);
        assert_eq!(
            r(other, &[other, SID_OWNER_RIGHTS]),
            [other],
            "без повторов"
        );
        assert_eq!(r(me, &["S-1-1-0", "S-1-1-0"]), ["S-1-1-0"]);
    }

    /// Разрешающие ACE с обратным вызовом (тип 9, SDDL `XA`) учитываются (M-2).
    #[test]
    fn callback_allow_ace_counted() {
        let dir = crate::test_util::dir_in_target("vault-acl-callback");
        let p = dir.join("k");
        std::fs::write(&p, "x").unwrap();
        set_dacl(
            &p,
            "D:P(A;;FA;;;OW)(XA;;FR;;;BU;(WIN://SYSAPPID Contains \"x\"))",
        )
        .unwrap();
        assert_eq!(foreign_readers(&p).unwrap(), ["S-1-5-32-545"]);
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
