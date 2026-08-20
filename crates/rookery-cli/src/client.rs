use serde::de::DeserializeOwned;

pub struct DaemonClient {
    base_url: String,
    client: reqwest::Client,
    api_key: Option<String>,
}

impl DaemonClient {
    pub fn new(base_url: &str, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            // connect_timeout only: an unreachable host otherwise sits in the
            // kernel SYN retry loop for ~130s. A total request timeout is
            // deliberately NOT set here — /api/start waits up to 120s on the
            // health check, /api/bench up to 60s, /api/stop up to
            // stop_timeout_secs. The bound on a wedged-but-listening daemon is
            // the 2s timeout on health(), which gates every command.
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(2))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            api_key: api_key
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty()),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .with_auth(self.client.get(&url))
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.ok();
            return Err(ClientError::Status { status, body });
        }

        resp.json()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    pub async fn post<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ClientError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .with_auth(self.client.post(&url))
            .json(body)
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.ok();
            return Err(ClientError::Status { status, body });
        }

        resp.json()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    fn with_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = &self.api_key {
            request.bearer_auth(api_key)
        } else {
            request
        }
    }

    pub async fn health(&self) -> bool {
        self.client
            .get(format!("{}/api/health", self.base_url))
            // health() is the first call in every command, so this is what
            // bounds a daemon that accepts the connection but never answers.
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection failed: {0} (is rookeryd running?)")]
    Connection(String),

    #[error("server returned status {status}{}", body.as_deref().map(str::trim).filter(|b| !b.is_empty()).map_or(String::new(), |b| format!(": {b}")))]
    Status { status: u16, body: Option<String> },

    #[error("response parse error: {0}")]
    Parse(String),
}
