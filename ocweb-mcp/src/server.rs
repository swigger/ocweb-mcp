use crate::{
    auth::PROTOCOL,
    browser::{Action, Browser},
    mcp::BrowserMcp,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, session::local::LocalSessionManager, tower::StreamableHttpService,
};
use serde_json::{Value, json};

pub fn router(browser: Browser, port: u16) -> Router {
    let service_browser = browser.clone();
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = browser.relay.shutdown.child_token();
    let service = StreamableHttpService::new(
        move || Ok(BrowserMcp::new(service_browser.clone())),
        LocalSessionManager::default().into(),
        config,
    );
    let protected = Router::new()
        .route("/api/tabs", get(tabs))
        .route("/api/browser", post(execute))
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(browser.clone(), authorize));
    Router::new()
        .route(
            "/",
            get(|| async { axum::response::Redirect::temporary("/docs") }),
        )
        .route("/health", get(health))
        .route(
            "/docs",
            get(|| async {
                (
                    [("cache-control", "no-store"), ("x-frame-options", "DENY")],
                    Html(include_str!("../static/docs.html")),
                )
            }),
        )
        .route(
            "/setup",
            get(move |State(browser): State<Browser>| async move {
                (
                    [("cache-control", "no-store")],
                    Json(json!({
                        "pairing": browser.relay.credentials.pairing(port),
                        "browser_connected": *browser.relay.connected.borrow(),
                    })),
                )
            }),
        )
        .route("/openapi.json", get(openapi))
        .route("/extension", get(extension))
        .merge(protected)
        .with_state(browser)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn(
            move |req: Request, next: Next| async move {
                let valid_host = req
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|h| {
                        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
                    });
                let extension_path = req.uri().path() == "/extension";
                let valid_origin = req
                    .headers()
                    .get("origin")
                    .map(|v| {
                        v.to_str().ok().is_some_and(|o| {
                            if extension_path {
                                o.strip_prefix("chrome-extension://").is_some_and(|id| {
                                    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
                                })
                            } else {
                                o == format!("http://127.0.0.1:{port}")
                                    || o == format!("http://localhost:{port}")
                            }
                        })
                    })
                    .unwrap_or(true);
                if !valid_host || !valid_origin {
                    return (
                        StatusCode::FORBIDDEN,
                        Json(json!({"error":"Host or Origin is not allowed"})),
                    )
                        .into_response();
                }
                next.run(req).await
            },
        ))
}
async fn authorize(
    State(browser): State<Browser>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !browser.relay.credentials.api_authorized(auth) {
        return (
            StatusCode::UNAUTHORIZED,
            [("www-authenticate", "Bearer")],
            Json(json!({"error":"Supply Authorization: Bearer <api_token from credentials.json>"})),
        )
            .into_response();
    }
    next.run(request).await
}
async fn health(State(browser): State<Browser>) -> Json<Value> {
    Json(
        json!({"service":"ocweb-mcp","version":env!("CARGO_PKG_VERSION"),"browser_connected":*browser.relay.connected.borrow()}),
    )
}
async fn tabs(State(browser): State<Browser>) -> Response {
    run(browser, Action::Tabs).await
}
async fn execute(
    State(browser): State<Browser>,
    action: Result<Json<Action>, axum::extract::rejection::JsonRejection>,
) -> Response {
    match action {
        Ok(Json(action)) => run(browser, action).await,
        Err(error) => (error.status(), Json(json!({"error":error.body_text()}))).into_response(),
    }
}
async fn run(browser: Browser, action: Action) -> Response {
    match browser.execute(action).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            let status = if *browser.relay.connected.borrow() {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            (status, Json(crate::relay::error_json(&e))).into_response()
        }
    }
}
async fn extension(
    State(browser): State<Browser>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !headers
        .get("sec-websocket-protocol")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| s.split(',').any(|s| s.trim() == PROTOCOL))
    {
        return (
            StatusCode::BAD_REQUEST,
            "Browser Relay Authentication v2 is required",
        )
            .into_response();
    }
    ws.protocols([PROTOCOL])
        .max_message_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| async move {
            if let Err(error) = browser.relay.serve_extension(socket).await {
                eprintln!("扩展连接结束: {error:#}");
            }
        })
}
async fn openapi() -> Json<Value> {
    let mut schema =
        serde_json::to_value(schemars::schema_for!(Action)).expect("schema serializes");
    // Keep JSON Schema references together in a single OpenAPI 3.1 schema resource.
    let mut value = json!({
        "openapi":"3.1.0","info":{"title":"OCWeb Browser API","version":env!("CARGO_PKG_VERSION"),"description":"Controls authorized Chrome tabs. Local-only. Use the same bearer token for REST and MCP."},
        "servers":[{"url":"/"}],
        "components":{"securitySchemes":{"bearerAuth":{"type":"http","scheme":"bearer"}}},
        "paths":{
            "/health":{"get":{"summary":"Connection status","responses":{"200":{"description":"Service and browser status"}}}},
            "/api/tabs":{"get":{"summary":"List authorized tabs","security":[{"bearerAuth":[]}],"responses":{"200":{"description":"Tab inventory"},"401":{"description":"Missing or invalid token"},"503":{"description":"Browser disconnected"}}}},
            "/api/browser":{"post":{"summary":"Execute a browser action","security":[{"bearerAuth":[]}],"requestBody":{"required":true,"content":{"application/json":{"schema":{}}}},"responses":{"200":{"description":"Action result (screenshot: mime_type and base64 data)"},"401":{"description":"Invalid token"},"422":{"description":"Invalid action input"},"502":{"description":"Browser command failed; inspect state before retrying mutations"},"503":{"description":"Browser disconnected"}}}}
        }
    });
    // Schemars uses root-local $defs; rewrite them to the embedded action resource.
    let raw = schema
        .to_string()
        .replace("#/$defs/", "#/components/schemas/Action/$defs/");
    schema = serde_json::from_str(&raw).expect("rewritten schema is JSON");
    value["components"]["schemas"] = json!({"Action":schema});
    value["paths"]["/api/browser"]["post"]["requestBody"]["content"]["application/json"]["schema"] =
        json!({"$ref":"#/components/schemas/Action"});
    Json(value)
}
