//! Passwords and key passphrases in the keychain of the operating system:
//! Windows Credential Manager, macOS Keychain, or the Secret Service on Linux
//! (GNOME Keyring, KWallet). gridwatch itself never writes them to a file.
//!
//! macOS asks the user for permission per keychain item, and again after every update
//! of a program that is not signed by a registered developer. With one item per server
//! that meant one question per server, so on macOS all secrets share a single item.
//! Elsewhere every server keeps its own entry; Windows limits the size of one entry.

use std::collections::HashMap;
use std::sync::Mutex;

const SERVICE: &str = "gridwatch";
/// The account name of the item that holds all secrets, as JSON of server id to secret.
const SHARED: &str = "all servers";
const ONE_ITEM: bool = cfg!(target_os = "macos");

/// The shared item as read once per run, so the keychain is asked only one time.
static SHARED_SECRETS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())
}

fn get(account: &str) -> Option<String> {
    entry(account).ok()?.get_password().ok()
}

fn set(account: &str, secret: &str) -> Result<(), String> {
    entry(account)?.set_password(secret).map_err(|e| e.to_string())
}

/// Forgetting something that was never stored is fine.
fn remove(account: &str) {
    if let Ok(entry) = entry(account) {
        let _ = entry.delete_credential();
    }
}

/// Run `change` on the secrets of the shared item and write them back if it says so.
fn with_shared<T>(change: impl FnOnce(&mut HashMap<String, String>) -> (T, bool)) -> Result<T, String> {
    let mut cache = SHARED_SECRETS.lock().unwrap();
    let secrets = cache.get_or_insert_with(|| {
        get(SHARED)
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    });
    let (result, changed) = change(secrets);
    if changed {
        if secrets.is_empty() {
            remove(SHARED);
        } else {
            set(SHARED, &serde_json::to_string(secrets).map_err(|e| e.to_string())?)?;
        }
    }
    Ok(result)
}

fn load_from(one_item: bool, server_id: &str) -> Option<String> {
    if !one_item {
        return get(server_id);
    }
    if let Some(secret) = with_shared(|all| (all.get(server_id).cloned(), false)).ok()? {
        return Some(secret);
    }
    // An entry of its own, written before all secrets shared one item: move it over.
    let old = get(server_id)?;
    let moved = with_shared(|all| {
        all.insert(server_id.to_string(), old.clone());
        ((), true)
    });
    if moved.is_ok() {
        remove(server_id);
    }
    Some(old)
}

fn save_to(one_item: bool, server_id: &str, secret: &str) -> Result<(), String> {
    if !one_item {
        return set(server_id, secret);
    }
    with_shared(|all| {
        let changed = all.get(server_id).map(String::as_str) != Some(secret);
        all.insert(server_id.to_string(), secret.to_string());
        ((), changed)
    })
}

fn delete_from(one_item: bool, server_id: &str) {
    if one_item {
        let _ = with_shared(|all| ((), all.remove(server_id).is_some()));
    }
    remove(server_id);
}

pub fn load(server_id: &str) -> Option<String> {
    load_from(ONE_ITEM, server_id)
}

pub fn save(server_id: &str, secret: &str) -> Result<(), String> {
    save_to(ONE_ITEM, server_id, secret)
}

pub fn delete(server_id: &str) {
    delete_from(ONE_ITEM, server_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes to the real keychain of this computer: `cargo test keychain -- --ignored`
    #[test]
    #[ignore]
    fn keychain_roundtrip() {
        let id = "gridwatch-selftest";
        save_to(false, id, "s3cret ä").unwrap();
        assert_eq!(load_from(false, id).as_deref(), Some("s3cret ä"));
        delete_from(false, id);
        assert_eq!(load_from(false, id), None);
    }

    /// The same, the way macOS does it: every secret in one shared item.
    /// Uses the real shared item and takes its own test entries out again.
    #[test]
    #[ignore]
    fn keychain_shared_item() {
        let (a, b, old) = ("gridwatch-selftest-a", "gridwatch-selftest-b", "gridwatch-selftest-old");
        // A secret stored the old way is found and moved into the shared item.
        set(old, "from before").unwrap();
        assert_eq!(load_from(true, old).as_deref(), Some("from before"));
        assert_eq!(get(old), None);

        save_to(true, a, "one").unwrap();
        save_to(true, b, "two ä").unwrap();
        let stored: HashMap<String, String> = serde_json::from_str(&get(SHARED).unwrap()).unwrap();
        assert_eq!(stored.len(), 3);
        assert_eq!(load_from(true, b).as_deref(), Some("two ä"));

        // What a new start reads from the keychain.
        *SHARED_SECRETS.lock().unwrap() = None;
        assert_eq!(load_from(true, a).as_deref(), Some("one"));
        assert_eq!(load_from(true, "gridwatch-selftest-unknown"), None);

        for id in [a, b, old] {
            delete_from(true, id);
        }
        assert_eq!(get(SHARED), None);
    }
}
