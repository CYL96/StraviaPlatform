use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use stravia_core::Gateway;

/// Authenticated admin export: Prometheus text exposition format version 0.0.4.
pub(super) async fn metrics(State(gateway): State<Gateway>) -> Response {
    let response = match gateway.admin().performance_metrics() {
        Some(body) => (StatusCode::OK, body).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            "performance recorder unavailable",
        )
            .into_response(),
    };
    let mut response = response;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        "text/plain; version=0.0.4; charset=utf-8"
            .parse()
            .expect("static MIME"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"stravia-performance.prom\""
            .parse()
            .expect("static filename"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static policy"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().expect("static policy"),
    );
    response
}

/// 管理员导出本轮 Chrome Trace；磁盘历史读取不占用异步执行线程。
pub(super) async fn timeline(State(gateway): State<Gateway>) -> Response {
    let snapshot =
        tokio::task::spawn_blocking(move || gateway.admin().performance_timeline()).await;
    let mut response = match snapshot {
        Ok(snapshot) => axum::Json(snapshot).into_response(),
        Err(error) => {
            tracing::warn!(%error, "performance timeline export task failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "performance timeline unavailable",
            )
                .into_response()
        }
    };
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"stravia-performance-trace.json\""
            .parse()
            .expect("static filename"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("static policy"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().expect("static policy"),
    );
    response
}
