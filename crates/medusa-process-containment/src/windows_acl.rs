//! Owner-only Windows file ACL operations without spawning system utilities.

use std::{
    ffi::c_void,
    io, iter,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr::{addr_of, null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE,
        Authorization::{
            EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW,
            SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, INHERITED_ACE, IsValidAcl, IsValidSid, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::FILE_ALL_ACCESS,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

/// Replaces `path`'s DACL with one protected full-control grant for the current user.
///
/// The descriptor is verified after it is written. Any missing, inherited, broad,
/// foreign, or otherwise unexpected access entry is rejected.
pub fn secure_current_user_only(path: &Path, directory: bool) -> io::Result<()> {
    let identity = CurrentUserSid::read()?;
    let inheritance = expected_inheritance(directory);
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: identity.sid().cast(),
        },
    };
    let mut acl = null_mut();
    // SAFETY: `access` and its SID remain alive for the call, `acl` is an out
    // pointer, and the returned allocation is released with `LocalFree`.
    let status = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
    if status != ERROR_SUCCESS {
        return Err(win32_status(status));
    }
    let acl = LocalAllocation::new(acl.cast()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows returned no ACL for the daemon endpoint",
        )
    })?;
    let mut path = wide_path(path);
    // SAFETY: `path` is mutable and NUL-terminated, `acl` points to a valid ACL,
    // and the remaining optional security-descriptor fields are intentionally null.
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl.as_ptr().cast(),
            null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_status(status));
    }
    verify_with_sid(path.as_mut_slice(), identity.sid(), directory)
}

/// Verifies that `path` has exactly one protected full-control ACE for the current user.
pub fn verify_current_user_only(path: &Path, directory: bool) -> io::Result<()> {
    let identity = CurrentUserSid::read()?;
    let mut path = wide_path(path);
    verify_with_sid(path.as_mut_slice(), identity.sid(), directory)
}

fn verify_with_sid(path: &mut [u16], expected_sid: PSID, directory: bool) -> io::Result<()> {
    let mut acl = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: `path` is mutable and NUL-terminated. `acl` and `descriptor` are
    // out pointers; the descriptor owns the returned ACL and is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_mut_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(win32_status(status));
    }
    let _descriptor = LocalAllocation::new(descriptor).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "daemon endpoint security descriptor is missing",
        )
    })?;
    if acl.is_null() {
        return Err(policy_error("daemon endpoint has a null DACL"));
    }
    // SAFETY: `acl` points inside the live descriptor returned above.
    if unsafe { IsValidAcl(acl) } == 0 {
        return Err(policy_error("daemon endpoint DACL is invalid"));
    }

    let mut control = 0;
    let mut revision = 0;
    // SAFETY: the descriptor remains live and both scalar out pointers are valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(policy_error("daemon endpoint DACL inherits permissions"));
    }

    // SAFETY: the ACL was validated and remains owned by the live descriptor.
    let ace_count = unsafe { (*acl).AceCount };
    if ace_count != 1 {
        return Err(policy_error(
            "daemon endpoint DACL must contain exactly one access entry",
        ));
    }
    let mut raw_ace: *mut c_void = null_mut();
    // SAFETY: index zero exists because `AceCount` is exactly one.
    if unsafe { GetAce(acl, 0, &mut raw_ace) } == 0 || raw_ace.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw_ace` points to an ACE whose header is readable. The type is
    // checked before fields specific to `ACCESS_ALLOWED_ACE` are interpreted.
    let header = unsafe { &*(raw_ace.cast::<windows_sys::Win32::Security::ACE_HEADER>()) };
    if header.AceType != ACCESS_ALLOWED_ACE_TYPE {
        return Err(policy_error("daemon endpoint ACE is not an allow entry"));
    }
    let expected_flags = expected_inheritance(directory) as u8;
    if header.AceFlags != expected_flags || header.AceFlags & INHERITED_ACE as u8 != 0 {
        return Err(policy_error(
            "daemon endpoint ACE has unexpected inheritance flags",
        ));
    }
    if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
        return Err(policy_error("daemon endpoint ACE is truncated"));
    }
    // SAFETY: the validated ACE type and size match `ACCESS_ALLOWED_ACE`.
    let ace = unsafe { &*(raw_ace.cast::<ACCESS_ALLOWED_ACE>()) };
    if ace.Mask & FILE_ALL_ACCESS != FILE_ALL_ACCESS {
        return Err(policy_error(
            "daemon endpoint ACE does not grant the current user full control",
        ));
    }
    let actual_sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
    // SAFETY: the SID is embedded in the size-validated ACE; the expected SID
    // remains backed by the live token-information buffer.
    if unsafe { IsValidSid(actual_sid) } == 0 || unsafe { EqualSid(actual_sid, expected_sid) } == 0
    {
        return Err(policy_error(
            "daemon endpoint ACL grants a principal other than the current user",
        ));
    }
    Ok(())
}

fn expected_inheritance(directory: bool) -> u32 {
    if directory {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT
    } else {
        NO_INHERITANCE
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect()
}

fn policy_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn win32_status(status: u32) -> io::Error {
    io::Error::from_raw_os_error(status as i32)
}

struct CurrentUserSid {
    buffer: Vec<usize>,
}

impl CurrentUserSid {
    fn read() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: `token` is a valid out pointer and the pseudo-process handle
        // returned by `GetCurrentProcess` does not require closing.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);
        let mut bytes = 0;
        // SAFETY: a null buffer with length zero is the documented sizing call.
        let sized = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut bytes) };
        if sized != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::last_os_error());
        }
        let words = (bytes as usize).div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words];
        // SAFETY: the aligned buffer has the exact byte capacity requested by
        // Windows and remains alive as part of `CurrentUserSid`.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let identity = Self { buffer };
        // SAFETY: `sid` points into the populated TOKEN_USER buffer.
        if unsafe { IsValidSid(identity.sid()) } == 0 {
            return Err(policy_error(
                "current process token contains an invalid SID",
            ));
        }
        Ok(identity)
    }

    fn sid(&self) -> PSID {
        // SAFETY: `buffer` is aligned and populated by `GetTokenInformation`
        // for `TokenUser`; it lives for the duration of the returned pointer.
        unsafe { (*(self.buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `OpenProcessToken` and is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl LocalAllocation {
    fn new(pointer: *mut c_void) -> Option<Self> {
        (!pointer.is_null()).then_some(Self(pointer))
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: this pointer was allocated by a Win32 API documented to use LocalAlloc.
        let _ = unsafe { LocalFree(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::{secure_current_user_only, verify_current_user_only};
    use tempfile::tempdir;

    #[test]
    fn secures_files_and_directories_for_only_the_current_user() {
        let temporary = tempdir().expect("temporary directory");
        let directory = temporary.path().join("ipc");
        std::fs::create_dir(&directory).expect("create directory");
        secure_current_user_only(&directory, true).expect("secure directory");
        verify_current_user_only(&directory, true).expect("verify directory");

        let endpoint = directory.join("endpoint.json");
        std::fs::write(&endpoint, b"{}").expect("create endpoint");
        secure_current_user_only(&endpoint, false).expect("secure endpoint");
        verify_current_user_only(&endpoint, false).expect("verify endpoint");
    }

    #[test]
    fn rejects_an_unprotected_inherited_acl() {
        let temporary = tempdir().expect("temporary directory");
        let endpoint = temporary.path().join("endpoint.json");
        std::fs::write(&endpoint, b"{}").expect("create endpoint");

        let error = verify_current_user_only(&endpoint, false)
            .expect_err("default inherited ACL must not satisfy owner-only policy");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }
}
