use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct Access {
    token: Option<String>,
    slots: Arc<tokio::sync::Semaphore>,
    budget: Arc<Mutex<(Instant, u32)>>,
    expensive_budget: Arc<Mutex<(Instant, u32)>>,
}

impl Access {
    pub fn from_env() -> Self {
        Self {
            token: configured_token(),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            budget: Arc::new(Mutex::new((Instant::now(), 0))),
            expensive_budget: Arc::new(Mutex::new((Instant::now(), 0))),
        }
    }
}

pub fn configured_token() -> Option<String> {
    std::env::var("STOCKSPOTTER_API_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn authorized(header: Option<&str>, token: Option<&str>) -> bool {
    let Some(token) = token else {
        return true;
    };
    let Some(value) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    // Constant work for equal-length credentials.
    value.len() == token.len()
        && value
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

pub async fn protect(State(access): State<Access>, req: Request, next: Next) -> Response {
    if !authorized(
        req.headers()
            .get("authorization")
            .and_then(|h| h.to_str().ok()),
        access.token.as_deref(),
    ) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if req.uri().path() == "/health" {
        return next.run(req).await;
    }
    {
        let mut budget = access.budget.lock().unwrap();
        if budget.0.elapsed() >= Duration::from_secs(60) {
            *budget = (Instant::now(), 0);
        }
        if budget.1 >= 120 {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        budget.1 += 1;
    }
    if matches!(req.uri().path(), "/assess" | "/movers/gainers") || req.uri().path().starts_with("/replay/signals/") {
        let mut budget = access.expensive_budget.lock().unwrap();
        if budget.0.elapsed() >= Duration::from_secs(60) {
            *budget = (Instant::now(), 0);
        }
        if budget.1 >= 6 {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        budget.1 += 1;
    }
    let Ok(_permit) = access.slots.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    match tokio::time::timeout(Duration::from_secs(120), next.run(req)).await {
        Ok(response) => response,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_exact_bearer_credential_when_configured() {
        assert!(!authorized(None, Some("secret")));
        assert!(!authorized(Some("Bearer wrong"), Some("secret")));
        assert!(authorized(Some("Bearer secret"), Some("secret")));
        assert!(authorized(None, None));
    }

    #[tokio::test]
    async fn http_auth_and_workload_limits_are_enforced() {
        let access = Access {
            token: Some("test-secret".into()),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
            budget: Arc::new(Mutex::new((Instant::now(), 0))),
            expensive_budget: Arc::new(Mutex::new((Instant::now(), 0))),
        };
        let app = axum::Router::new()
            .route("/test", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                access.clone(),
                protect,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/test", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest::Client::new();
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("test-secret")
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let permits = access.slots.acquire_many(4).await.unwrap();
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("test-secret")
                .send()
                .await
                .unwrap()
                .status(),
            429
        );
        drop(permits);
        access.budget.lock().unwrap().1 = 120;
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("test-secret")
                .send()
                .await
                .unwrap()
                .status(),
            429
        );
        server.abort();
    }
}
