//! `saveMessappKey`, `loadMessappKey`, `clearMessappKey`: the Friends tab's DM
//! private key, one per Messapp user, in the OS keyring (Electron used safeStorage).

const MESSAPP_URL: &str = "https://boxhdygkyopixoejztuk.supabase.co";
const MESSAPP_PUBLISHABLE_KEY: &str = "sb_publishable_DAB7bLq6qy7r9bdWu8empA_ED55mzMa";
const KEYRING_SERVICE: &str = "chimaera";

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

async fn entry(access_token: &str) -> Result<Option<keyring::Entry>, String> {
    let user_id = verify_session(access_token).await?;
    Ok(keyring::Entry::new(KEYRING_SERVICE, &format!("messapp-key:{user_id}")).ok())
}

/// False when the OS keyring is unavailable; the renderer then keeps the key in memory only.
pub async fn save(access_token: &str, private_jwk: &str) -> Result<bool, String> {
    Ok(entry(access_token).await?.is_some_and(|e| e.set_password(private_jwk).is_ok()))
}

pub async fn load(access_token: &str) -> Result<Option<String>, String> {
    Ok(entry(access_token).await?.and_then(|e| e.get_password().ok()))
}

pub async fn clear(access_token: &str) -> Result<(), String> {
    if let Some(e) = entry(access_token).await? {
        let _ = e.delete_credential();
    }
    Ok(())
}
