//! The server's HTTP side of a desktop: opening a pairing session, waiting for a phone to claim it, and renewing the access token.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("network: {0}")]
    Network(#[from] reqwest::Error),
    /// The server answered with an error; `code` is its machine-readable code when it sent one.
    #[error("server said {status} {code}: {message}")]
    Server {
        status: u16,
        code: String,
        message: String,
    },
}

impl ApiError {
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ApiError::Server { status: 401, .. })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingSession {
    pub session_id: String,
    pub secret: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    pub pairing_id: String,
    pub phone: ClaimPhone,
    pub pairing_token: String,
    pub device_access_token: String,
    pub device_refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaimPhone {
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Clone)]
pub enum PollResult {
    Waiting,
    Claimed(Box<Claim>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub access_token_expires_in: i64,
}

pub struct Api {
    base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(base: &str) -> Api {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("wiqo-desktop/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Api {
            base: base.trim_end_matches('/').to_string(),
            http,
        }
    }

    async fn parse<T: for<'de> Deserialize<'de>>(
        response: reqwest::Response,
    ) -> Result<T, ApiError> {
        let status = response.status();
        if status.is_success() {
            return Ok(response.json().await?);
        }
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        let error = &body["error"];
        Err(ApiError::Server {
            status: status.as_u16(),
            code: error["code"].as_str().unwrap_or("").to_string(),
            message: error["message"].as_str().unwrap_or("").to_string(),
        })
    }

    pub async fn create_pairing_session(
        &self,
        device_id: &str,
        display_name: &str,
        platform: &str,
    ) -> Result<PairingSession, ApiError> {
        let response = self.http.post(format!("{}/device-pairing/sessions", self.base)).json(&json!({ "deviceId": device_id, "displayName": display_name, "platform": platform })).send().await?;
        Self::parse(response).await
    }

    pub async fn poll_pairing(
        &self,
        session_id: &str,
        secret: &str,
    ) -> Result<PollResult, ApiError> {
        let response = self
            .http
            .post(format!(
                "{}/device-pairing/sessions/{}/poll",
                self.base, session_id
            ))
            .json(&json!({ "secret": secret }))
            .send()
            .await?;
        let value: serde_json::Value = Self::parse(response).await?;
        if value["status"] == "claimed" {
            let claim: Claim = serde_json::from_value(value).map_err(|error| ApiError::Server {
                status: 502,
                code: "BAD_CLAIM".into(),
                message: error.to_string(),
            })?;
            return Ok(PollResult::Claimed(Box::new(claim)));
        }
        Ok(PollResult::Waiting)
    }

    pub async fn refresh(&self, device_id: &str, refresh_token: &str) -> Result<Tokens, ApiError> {
        let response = self
            .http
            .post(format!("{}/device-sessions/refresh", self.base))
            .json(&json!({ "deviceId": device_id, "refreshToken": refresh_token }))
            .send()
            .await?;
        Self::parse(response).await
    }

    /// The signed-in account makes this computer one of its devices: the device session comes from the account, no phone needed.
    pub async fn bootstrap(
        &self,
        account_token: &str,
        device_id: &str,
    ) -> Result<Tokens, ApiError> {
        let response = self
            .http
            .post(format!("{}/device-sessions/bootstrap", self.base))
            .bearer_auth(account_token)
            .json(&json!({ "deviceId": device_id }))
            .send()
            .await?;
        Self::parse(response).await
    }

    /// Unpairs one phone on the server; the server tells this desktop back with `pairingRevoked`.
    pub async fn revoke_phone(&self, access_token: &str, pairing_id: &str) -> Result<(), ApiError> {
        let response = self
            .http
            .delete(format!(
                "{}/device-pairing/phones/{}",
                self.base, pairing_id
            ))
            .bearer_auth(access_token)
            .send()
            .await?;
        Self::parse::<serde_json::Value>(response).await.map(|_| ())
    }

    /// The relay socket address for this server.
    pub fn socket_url(&self) -> String {
        let swapped = if let Some(rest) = self.base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.base.clone()
        };
        format!("{swapped}/device/socket")
    }

    pub fn base(&self) -> &str {
        &self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_address_follows_the_server_address() {
        assert_eq!(
            Api::new("https://api.wiqoai.com/").socket_url(),
            "wss://api.wiqoai.com/device/socket"
        );
        assert_eq!(
            Api::new("http://127.0.0.1:3999").socket_url(),
            "ws://127.0.0.1:3999/device/socket"
        );
    }
}
