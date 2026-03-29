use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::app_state::AppState;

pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = {
        let config = state.config.read().await;
        normalized_key(config.api_key.as_deref())
            .map(str::to_owned)
            .filter(|key| !key.is_empty())
    };

    let Some(expected) = expected else {
        return Ok(next.run(request).await);
    };

    let provided = extract_token(&request);
    if provided
        .as_deref()
        .is_some_and(|candidate| constant_time_eq(candidate, &expected))
    {
        return Ok(next.run(request).await);
    }

    Err(StatusCode::UNAUTHORIZED)
}

fn extract_token(request: &Request) -> Option<String> {
    bearer_token(request)
        .map(str::to_owned)
        .or_else(|| sse_query_token(request))
}

fn bearer_token(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

fn sse_query_token(request: &Request) -> Option<String> {
    if !request.uri().path().ends_with("/events") {
        return None;
    }

    let query = request.uri().query()?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "token")
        .map(|(_, value)| value.into_owned())
}

fn normalized_key(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();

    let mut diff = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        let a = left.get(idx).copied().unwrap_or(0);
        let b = right.get(idx).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }

    diff == 0
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;
    use crate::routes;
    use crate::sse;
    use crate::test_utils::build_test_app_state;

    fn auth_router(state: Arc<AppState>) -> Router {
        let auth_layer = middleware::from_fn_with_state(state.clone(), require_api_key);
        let protected_api = Router::new()
            .route("/status", get(routes::get_status))
            .route("/events", get(sse::get_events))
            .route_layer(auth_layer);

        Router::new()
            .route("/api/health", get(routes::get_health))
            .route("/metrics", get(routes::get_metrics))
            .nest("/api", protected_api)
            .fallback(routes::get_dashboard)
            .with_state(state)
    }

    #[tokio::test]
    async fn test_health_is_public_when_auth_enabled() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_is_public_when_auth_enabled() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_protected_api_requires_bearer_token() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_protected_api_accepts_bearer_token() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header(header::AUTHORIZATION, "Bearer rky-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_sse_accepts_query_token() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/events?token=rky-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dashboard_shell_stays_public() {
        let (_dir, state) = build_test_app_state(None);
        state.config.write().await.api_key = Some("rky-secret".into());
        let app = auth_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
