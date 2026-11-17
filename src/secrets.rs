//! Passwords and key passphrases in the keychain of the operating system:
//! Windows Credential Manager, macOS Keychain, or the Secret Service on Linux
//! (GNOME Keyring, KWallet). gridwatch itself never writes them to a file.

const SERVICE: &str = "gridwatch";

fn entry(server_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, server_id).map_err(|e| e.to_string())
}

pub fn load(server_id: &str) -> Option<String> {
    entry(server_id).ok()?.get_password().ok()
}

pub fn save(server_id: &str, secret: &str) -> Result<(), String> {
    entry(server_id)?.set_password(secret).map_err(|e| e.to_string())
}

/// Forgetting something that was never stored is fine.
pub fn delete(server_id: &str) {
    if let Ok(entry) = entry(server_id) {
        let _ = entry.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    /// Writes to the real keychain of this computer: `cargo test keychain -- --ignored`
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        let id = "gridwatch-selftest";
        super::save(id, "s3cret ä").unwrap();
        assert_eq!(super::load(id).as_deref(), Some("s3cret ä"));
        super::delete(id);
        assert_eq!(super::load(id), None);
    }
}
