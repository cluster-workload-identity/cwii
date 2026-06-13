//! HTTP server: shared state, router, and the `/mutate` handler.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use cwii_core::{CoreConfig, Error, Provider, WebhookState, mutate};
use k8s_openapi::api::core::v1::Pod;
use kube::core::DynamicObject;
use kube::core::admission::AdmissionReview;

pub struct AppState {
    pub client: kube::Client,
    pub core: CoreConfig,
    pub providers: Vec<Box<dyn Provider>>,
}

impl WebhookState for AppState {
    fn client(&self) -> &kube::Client {
        &self.client
    }

    fn providers(&self) -> &[Box<dyn Provider>] {
        &self.providers
    }

    fn core(&self) -> &CoreConfig {
        &self.core
    }
}

pub type SharedState = Arc<AppState>;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/mutate", post(mutate_handler))
        .with_state(state)
}

async fn mutate_handler(
    State(state): State<SharedState>,
    Json(review): Json<AdmissionReview<Pod>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, Error> {
    let response = mutate(state.as_ref(), review).await?;
    Ok(Json(response))
}
