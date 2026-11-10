//! `saveMessappKey`, `loadMessappKey`, `clearMessappKey`: the Friends tab's DM keys,
//! in the OS keyring vault Messapp desktop and Purrch share (`shaiyon-keys`, service
//! `skibi`). A key Messapp desktop already saved on this PC unlocks Chimaera with no PIN.

use serde_json::{Value, json};
use shaiyon_crypto::Jwk;
use shaiyon_keys::{E2eeKeys, KeyVault, OsStore, SecretStore};

const MESSAPP_URL: &str = "https://boxhdygkyopixoejztuk.supabase.co";
const MESSAPP_PUBLISHABLE_KEY: &str = "sb_publishable_DAB7bLq6qy7r9bdWu8empA_ED55mzMa";
/// Where Chimaera kept the private key before the shared vault; moved over on first load.
const OLD_SERVICE: &str = "chimaera";

/// The renderer is untrusted, so the user id comes from Supabase, never from the caller.
async fn verify_session(access_token: &str) -> Result<String, String> {
    if access_token.is_empty() {
        return Err("Missing access token".into());
    }
    let user: serde_json::Value = reqwest::Client::new()
        .get(format!("{MESSAPP_URL}/auth/v1/user"))
        .bearer_auth(access_token)
        .header("apikey", MESSAPP_PUBLISHABLE_KEY)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    user["id"].as_str().filter(|id| !id.is_empty()).map(str::to_string).ok_or_else(|| "Session verification returned no user id".into())
}

fn old_entry(uid: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(OLD_SERVICE, &format!("messapp-key:{uid}")).ok()
}

/// The vault's keys; if it has no private key yet, `old` (Chimaera's pre-vault key) moves in.
fn migrate<S: SecretStore>(vault: &KeyVault<S>, uid: &str, old: Option<&str>) -> Result<E2eeKeys, String> {
    let mut keys = vault.load(uid).map_err(|e| e.to_string())?;
    if let (None, Some(old)) = (&keys.private, old.and_then(|o| serde_json::from_str::<Jwk>(o).ok())) {
        keys.public = Some(old.public());
        keys.private = Some(old);
        vault.save(uid, &keys).map_err(|e| e.to_string())?;
    }
    Ok(keys)
}

/// Sets the private key (and its public half), keeping the legacy keys Messapp stored.
fn store<S: SecretStore>(vault: &KeyVault<S>, uid: &str, private: Jwk) -> Result<(), String> {
    let mut keys = vault.load(uid).map_err(|e| e.to_string())?;
    keys.public = Some(private.public());
    keys.private = Some(private);
    vault.save(uid, &keys).map_err(|e| e.to_string())
}

/// False when the OS keyring is unavailable; the renderer then keeps the key in memory only.
pub async fn save(access_token: &str, private_jwk: &str) -> Result<bool, String> {
    let uid = verify_session(access_token).await?;
    let private: Jwk = serde_json::from_str(private_jwk).map_err(|_| "Invalid private key".to_string())?;
    if private.d.is_none() {
        return Err("Invalid private key".into());
    }
    Ok(store(&KeyVault::new(OsStore), &uid, private).is_ok())
}

/// `{ private, legacy }`: the private key plus the keys of earlier rotations, for old DMs.
pub async fn load(access_token: &str) -> Result<Option<Value>, String> {
    let uid = verify_session(access_token).await?;
    let old = old_entry(&uid);
    let old_key = old.as_ref().and_then(|e| e.get_password().ok());
    // An unreachable keyring reads as "no saved key": the renderer asks for the PIN.
    let Ok(keys) = migrate(&KeyVault::new(OsStore), &uid, old_key.as_deref()) else { return Ok(None) };
    if let (Some(_), Some(e)) = (&old_key, old) {
        let _ = e.delete_credential();
    }
    Ok(keys.private.map(|private| json!({ "private": private, "legacy": keys.legacy })))
}

/// Sign-out keeps the shared vault, as Messapp's does: Messapp desktop and Purrch on this
/// PC read the same keys. Only Chimaera's pre-vault entry is removed.
pub async fn clear(access_token: &str) -> Result<(), String> {
    let uid = verify_session(access_token).await?;
    if let Some(e) = old_entry(&uid) {
        let _ = e.delete_credential();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shaiyon_keys::MemoryStore;

    fn key() -> Jwk {
        shaiyon_crypto::generate_keypair()
    }

    #[test]
    fn migrate_moves_the_old_key_and_keeps_legacy() {
        let vault = KeyVault::new(MemoryStore::default());
        let (old, legacy) = (key(), key());
        vault.save("u1", &E2eeKeys { private: None, public: None, legacy: vec![legacy.clone()] }).unwrap();
        let keys = migrate(&vault, "u1", Some(&serde_json::to_string(&old).unwrap())).unwrap();
        assert_eq!(keys.private.as_ref(), Some(&old));
        assert_eq!(keys.public, Some(old.public()));
        assert_eq!(keys.legacy, vec![legacy]);
        assert_eq!(vault.load("u1").unwrap(), keys);
    }

    #[test]
    fn migrate_keeps_a_vault_key_and_ignores_junk() {
        let vault = KeyVault::new(MemoryStore::default());
        let current = key();
        store(&vault, "u1", current.clone()).unwrap();
        let keys = migrate(&vault, "u1", Some(&serde_json::to_string(&key()).unwrap())).unwrap();
        assert_eq!(keys.private, Some(current));
        assert_eq!(migrate(&vault, "u2", Some("not a key")).unwrap(), E2eeKeys::default());
        assert_eq!(migrate(&vault, "u2", None).unwrap(), E2eeKeys::default());
    }

    #[test]
    fn store_keeps_legacy_keys() {
        let vault = KeyVault::new(MemoryStore::default());
        let legacy = key();
        vault.save("u1", &E2eeKeys { private: Some(key()), public: None, legacy: vec![legacy.clone()] }).unwrap();
        let next = key();
        store(&vault, "u1", next.clone()).unwrap();
        let keys = vault.load("u1").unwrap();
        assert_eq!((keys.private, keys.public, keys.legacy), (Some(next.clone()), Some(next.public()), vec![legacy]));
    }
}
