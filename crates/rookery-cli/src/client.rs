use serde::de::DeserializeOwned;

pub struct DaemonClient {
    base_url: String,
    client: reqwest::Client,
}

impl DaemonClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ClientError::Status(resp.status().as_u16()));
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
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| ClientError::Connection(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(ClientError::Status(resp.status().as_u16()));
        }

        resp.json()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    pub async fn health(&self) -> bool {
        self.client
            .get(format!("{}/api/health", self.base_url))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection failed: {0} (is rookeryd running?)")]
    Connection(String),

    #[error("server returned status {0}")]
    Status(u16),

    #[error("response parse error: {0}")]
    Parse(String),
}
