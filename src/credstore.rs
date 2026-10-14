// fastlaunch - a fast game launcher front end
// Copyright (C) 2026 marcin
// SPDX-License-Identifier: GPL-3.0-only

//! Small wrapper around Windows Credential Manager, for the Epic sign-in.
//! The secret never touches a plain file.

use std::ptr;

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn save(target: &str, secret: &str) -> std::io::Result<()> {
    let mut target_w = wide(target);
    let mut user_w = wide("fastlaunch");
    let mut blob = secret.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target_w.as_mut_ptr(),
        Comment: ptr::null_mut(),
        LastWritten: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
        CredentialBlobSize: u32::try_from(blob.len())
            .map_err(|_| std::io::Error::other("secret too large"))?,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: ptr::null_mut(),
        TargetAlias: ptr::null_mut(),
        UserName: user_w.as_mut_ptr(),
    };
    // SAFETY: every pointer in `cred` points into a buffer that outlives the call.
    let ok = unsafe { CredWriteW(&cred, 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub fn load(target: &str) -> Option<String> {
    let target_w = wide(target);
    let mut cred: *mut CREDENTIALW = ptr::null_mut();
    // SAFETY: `target_w` is NUL-terminated; on success `cred` is freed with CredFree below.
    let ok = unsafe { CredReadW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) };
    if ok == 0 || cred.is_null() {
        return None;
    }
    // SAFETY: CredReadW succeeded, so `cred` points to a valid CREDENTIALW whose blob
    // is `CredentialBlobSize` bytes long.
    unsafe {
        let c = &*cred;
        let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let secret = String::from_utf8(bytes.to_vec()).ok();
        CredFree(cred.cast());
        secret
    }
}

pub fn delete(target: &str) {
    let target_w = wide(target);
    // SAFETY: `target_w` is NUL-terminated. A missing entry is not an error for us.
    unsafe {
        CredDeleteW(target_w.as_ptr(), CRED_TYPE_GENERIC, 0);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trips_through_credential_manager() {
        let target = "fastlaunch/selftest";
        super::save(target, "secret-value").unwrap();
        assert_eq!(super::load(target).as_deref(), Some("secret-value"));
        super::delete(target);
        assert_eq!(super::load(target), None);
    }
}
