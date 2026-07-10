use axum::{
    extract::{Request, State},
    http::{StatusCode, header::WWW_AUTHENTICATE},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_auth::AuthBasic;

use super::AppStateRef;

/// 401 with a WWW-Authenticate challenge so browsers show their native
/// login prompt (needed for /panel); curl/API clients keep working with -u.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, "Basic realm=\"rust-mumble\"")],
        "unauthorized",
    )
        .into_response()
}

pub async fn auth_basic(
    State(auth_state): State<AppStateRef>,
    // Option<> so a missing/malformed Authorization header lands here as None
    // instead of being rejected by the extractor without a challenge.
    auth: Option<AuthBasic>,
    request: Request,
    next: Next,
) -> Response {
    if auth_state.auth.password.is_none() {
        return unauthorized();
    }

    if let Some(AuthBasic((id, password))) = auth {
        if id == auth_state.auth.username && password == auth_state.auth.password {
            return next.run(request).await;
        }
    }

    unauthorized()
}
