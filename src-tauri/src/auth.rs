use serde::{Deserialize, Serialize};
use crate::error::{LauncherError, Result};

/// Microsoft OAuth2 token response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MicrosoftToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// Xbox Live token response
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct XboxToken {
    pub token: String,
    pub user_hash: String,
}

/// Minecraft access token
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MinecraftToken {
    pub access_token: String,
    pub expires_in: u64,
}

/// User profile from Minecraft services
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MinecraftProfile {
    pub id: String,
    pub name: String,
}

/// Complete auth state persisted to disk
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct AuthState {
    pub microsoft_token: Option<MicrosoftToken>,
    pub minecraft_token: Option<MinecraftToken>,
    pub profile: Option<MinecraftProfile>,
    #[serde(default)]
    pub timestamp: u64,
    
    /// Whether this is an offline mode token (cached credentials)
    pub offline_mode: bool,
}


/// Authenticate with Microsoft OAuth2 Device Code Flow
/// This flow is simpler and doesn't require a redirect server
pub async fn start_device_code_flow(client_id: &str) -> Result<serde_json::Value> {
    let client = crate::download::global_http_client();
    let resp = client
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
        .form(&[
            ("client_id", client_id),
            ("scope", "XboxLive.SignIn XboxLive.offline_access"),
        ])
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if resp.get("error").is_some() {
        return Err(LauncherError::Auth(
            resp["error_description"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string(),
        ));
    }

    Ok(resp)
}

/// Poll for device code authorization completion
pub async fn poll_device_code(
    client_id: &str,
    device_code: &str,
) -> Result<MicrosoftToken> {
    let client = crate::download::global_http_client();
    let resp = client
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if let Some(error) = resp.get("error") {
        let error_str = error.as_str().unwrap_or("");
        if error_str == "authorization_pending" {
            return Err(LauncherError::Auth("authorization_pending".into()));
        }
        return Err(LauncherError::Auth(
            resp["error_description"]
                .as_str()
                .unwrap_or("Unknown error")
                .to_string(),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(MicrosoftToken {
        access_token: resp["access_token"]
            .as_str()
            .ok_or_else(|| LauncherError::Auth("Missing access_token".into()))?
            .to_string(),
        refresh_token: resp["refresh_token"]
            .as_str()
            .ok_or_else(|| LauncherError::Auth("Missing refresh_token".into()))?
            .to_string(),
        expires_in: now + resp["expires_in"].as_u64().unwrap_or(3600),
    })
}

/// Refresh Microsoft token
pub async fn refresh_microsoft_token(
    client_id: &str,
    refresh_token: &str,
) -> Result<MicrosoftToken> {
    let client = crate::download::global_http_client();
    let resp = client
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", "XboxLive.SignIn XboxLive.offline_access"),
        ])
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if resp.get("error").is_some() {
        return Err(LauncherError::Auth("Token refresh failed".into()));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(MicrosoftToken {
        access_token: resp["access_token"].as_str().unwrap_or("").to_string(),
        refresh_token: resp["refresh_token"].as_str().unwrap_or("").to_string(),
        expires_in: now + resp["expires_in"].as_u64().unwrap_or(86400),
    })
}

/// Exchange Microsoft token for Xbox Live token
pub async fn get_xbox_token(ms_token: &str) -> Result<XboxToken> {
    let client = crate::download::global_http_client();
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={}", ms_token)
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let resp = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("x-xbl-contract-version", "1")
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let token = resp["Token"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing Xbox token".into()))?
        .to_string();
    let user_hash = resp["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing user hash".into()))?
        .to_string();

    Ok(XboxToken { token, user_hash })
}

/// Exchange Xbox Live token for XSTS token
pub async fn get_xsts_token(xbox_token: &str) -> Result<XboxToken> {
    let client = crate::download::global_http_client();
    let body = serde_json::json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbox_token]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });

    let resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("x-xbl-contract-version", "1")
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if let Some(xerr) = resp.get("XErr") {
        let code = xerr.as_u64().unwrap_or(0);
        let msg = match code {
            2148916233 => "No Xbox account found. Please create one.",
            2148916235 => "Xbox Live is not available in your country.",
            2148916236 | 2148916237 => "Adult verification needed.",
            2148916238 => "Child account. Add to a Family.",
            _ => "Unknown XSTS error",
        };
        return Err(LauncherError::Auth(msg.into()));
    }

    let token = resp["Token"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing XSTS token".into()))?
        .to_string();
    let user_hash = resp["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing user hash".into()))?
        .to_string();

    Ok(XboxToken { token, user_hash })
}

/// Exchange XSTS token for Minecraft token
pub async fn get_minecraft_token(xsts_token: &str, user_hash: &str) -> Result<MinecraftToken> {
    let client = crate::download::global_http_client();
    let body = serde_json::json!({
        "xtoken": format!("XBL3.0 x={};{}", user_hash, xsts_token),
        "platform": "PC_LAUNCHER"
    });

    let resp = client
        .post("https://api.minecraftservices.com/launcher/login")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(MinecraftToken {
        access_token: resp["access_token"]
            .as_str()
            .ok_or_else(|| LauncherError::Auth("Missing MC token".into()))?
            .to_string(),
        expires_in: now + resp["expires_in"].as_u64().unwrap_or(86400),
    })
}

/// Verify game ownership
pub async fn check_ownership(mc_token: &str) -> Result<bool> {
    let client = crate::download::global_http_client();
    let resp = client
        .get("https://api.minecraftservices.com/entitlements/license")
        .header("Authorization", format!("Bearer {}", mc_token))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let items = resp["items"].as_array();
    Ok(items.is_some_and(|items| !items.is_empty()))
}

/// Get Minecraft profile (username + UUID)
pub async fn get_profile(mc_token: &str) -> Result<MinecraftProfile> {
    let client = crate::download::global_http_client();
    let resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .header("Authorization", format!("Bearer {}", mc_token))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if resp.get("error").is_some() {
        return Err(LauncherError::Auth(
            "No Minecraft profile found. Do you own the game?".into(),
        ));
    }

    Ok(MinecraftProfile {
        id: resp["id"].as_str().unwrap_or("").to_string(),
        name: resp["name"].as_str().unwrap_or("").to_string(),
    })
}

/// Full authentication flow: Microsoft → Xbox → XSTS → Minecraft
pub async fn full_auth_flow(ms_token: &MicrosoftToken) -> Result<(MinecraftToken, MinecraftProfile)> {
    let xbox = get_xbox_token(&ms_token.access_token).await?;
    let xsts = get_xsts_token(&xbox.token).await?;
    let mc_token = get_minecraft_token(&xsts.token, &xsts.user_hash).await?;

    let owns_game = check_ownership(&mc_token.access_token).await?;
    if !owns_game {
        return Err(LauncherError::Auth(
            "You don't own Minecraft. Please purchase the game.".into(),
        ));
    }

    let profile = get_profile(&mc_token.access_token).await?;
    Ok((mc_token, profile))
}

/// Save auth state to disk
pub fn save_auth_state(path: &std::path::Path, state: &AuthState) -> Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Check if a Minecraft token is expired (with a 5-minute buffer).
pub fn is_token_expired(token: &MinecraftToken) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now >= token.expires_in.saturating_sub(300)
}

/// Check if we can launch in offline mode using cached credentials
pub fn can_launch_offline(path: &std::path::Path) -> bool {
    if let Some(state) = load_auth_state(path) {
        // Check if we have a valid Minecraft token (not expired)
        if let Some(ref token) = state.minecraft_token {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            
            // Token is valid if not expired (with 5 minute buffer)
            if now < token.expires_in.saturating_sub(300) {
                return true;
            }
        }
    }
    false
}

/// Get offline mode credentials (username and UUID) from cached auth state
pub fn get_offline_credentials(path: &std::path::Path) -> Option<(String, String)> {
    if let Some(state) = load_auth_state(path) {
        if let (Some(ref token), Some(ref profile)) = (&state.minecraft_token, &state.profile) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            
            // Check if token is still valid (not expired)
            if now < token.expires_in.saturating_sub(300) {
                return Some((profile.name.clone(), profile.id.clone()));
            }
        }
    }
    None
}

/// Load auth state from disk
pub fn load_auth_state(path: &std::path::Path) -> Option<AuthState> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Ely.by authentication
pub async fn elyby_login(username: &str, password: &str) -> Result<(String, String, String)> {
    let client = crate::download::global_http_client();
    let body = serde_json::json!({
        "username": username,
        "password": password,
        "clientToken": uuid::Uuid::new_v4().to_string(),
        "agent": {
            "name": "Minecraft",
            "version": 1
        }
    });

    let resp = client
        .post("https://authserver.ely.by/auth/authenticate")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    if resp.get("error").is_some() {
        return Err(LauncherError::Auth(
            resp["errorMessage"]
                .as_str()
                .unwrap_or("Ely.by authentication failed")
                .to_string()
        ));
    }

    let access_token = resp["accessToken"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing accessToken".into()))?
        .to_string();
    let uuid = resp["selectedProfile"]["id"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing profile id".into()))?
        .to_string();
    let name = resp["selectedProfile"]["name"]
        .as_str()
        .ok_or_else(|| LauncherError::Auth("Missing profile name".into()))?
        .to_string();

    Ok((name, uuid, access_token))
}

/// Change skin for Microsoft account
pub async fn change_microsoft_skin(mc_token: &str, skin_path: &std::path::Path, variant: &str) -> Result<()> {
    let client = crate::download::global_http_client();
    let skin_data = std::fs::read(skin_path)?;

    let boundary = "----VoidLauncherSkinBoundary";
    let variant_str = if variant == "slim" { "SLIM" } else { "CLASSIC" };

    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"variant\"\r\n\r\n{variant_str}\r\n").as_bytes()
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"skin.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes()
    );
    body.extend_from_slice(&skin_data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let resp = client
        .post("https://api.minecraftservices.com/minecraft/profile/skins")
        .header("Authorization", format!("Bearer {}", mc_token))
        .header("Content-Type", format!("multipart/form-data; boundary={boundary}"))
        .body(body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(LauncherError::Auth(format!("Failed to change skin: {}", text)));
    }

    Ok(())
}
