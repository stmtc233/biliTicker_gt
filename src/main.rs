// main.rs

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::task;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;

// 引入你的模块
mod abstraction;
mod click;
mod error;
mod slide;
mod w;

use crate::abstraction::{Api, GenerateW, Test, VerifyType};
use crate::click::Click;
use crate::slide::Slide;

// --- 新增：客户端管理器 ---
// 用于缓存和重用 reqwest::Client 实例
#[derive(Clone)]
struct ClientManager {
    // Key 是代理 URL，或者 "default" 代表无代理
    // Value 是一个共享的 Client 实例
    clients: Arc<Mutex<HashMap<String, Arc<Client>>>>,
}

impl ClientManager {
    fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // 获取或创建一个客户端
    fn get(&self, proxy: Option<&str>) -> Result<Arc<Client>, crate::error::Error> {
        let key = proxy.unwrap_or("default").to_string();
        let mut clients = self.clients.lock().expect("ClientManager mutex poisoned");

        // 如果客户端已存在，则克隆其 Arc 指针并返回
        if let Some(client) = clients.get(&key) {
            return Ok(Arc::clone(client));
        }

        // 否则，创建一个新的客户端
        let client_builder = Client::builder();
        let new_client = match proxy {
            Some(proxy_url) => {
                let proxy = reqwest::Proxy::all(proxy_url)
                    .map_err(|e| error::other("无效的代理 URL", e))?;
                client_builder
                    .proxy(proxy)
                    .build()
                    .map_err(|e| error::other("构建代理客户端失败", e))?
            }
            None => client_builder
                .build()
                .map_err(|e| error::other("构建默认客户端失败", e))?,
        };

        let client_arc = Arc::new(new_client);
        clients.insert(key, Arc::clone(&client_arc));
        Ok(client_arc)
    }
}

// --- 修改后的应用状态 ---
#[derive(Clone)]
struct AppState {
    client_manager: ClientManager,
    click_instances: Arc<Mutex<HashMap<String, Click>>>,
    slide_instances: Arc<Mutex<HashMap<String, Slide>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            client_manager: ClientManager::new(),
            click_instances: Arc::new(Mutex::new(HashMap::new())),
            slide_instances: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// --- 响应结构体保持不变 ---
#[derive(Deserialize)]
struct SimpleMatchRequest {
    gt: String,
    challenge: String,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct RegisterTestRequest {
    url: String,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct GetCSRequest {
    gt: String,
    challenge: String,
    w: Option<String>,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct GetTypeRequest {
    gt: String,
    challenge: String,
    w: Option<String>,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct VerifyRequest {
    gt: String,
    challenge: String,
    w: Option<String>,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct GenerateWRequest {
    key: String,
    gt: String,
    challenge: String,
    c: Vec<u8>,
    s: String,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Deserialize)]
struct TestRequest {
    url: String,
    session_id: Option<String>,
    proxy: Option<String>,
}

#[derive(Serialize)]
struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Serialize)]
struct TupleResponse2 {
    first: String,
    second: String,
}

#[derive(Serialize)]
struct CSResponse {
    c: Vec<u8>,
    s: String,
}

impl<T> ApiResponse<T> {
    fn success(data: T) -> Self {
        Self { success: true, data: Some(data), error: None }
    }
    fn error(message: String) -> Self {
        Self { success: false, data: None, error: Some(message) }
    }
}
fn get_click_instance(
    state: &AppState,
    session_id: Option<String>,
    proxy: Option<String>,
) -> Result<Click, Response> {
    let session_id = session_id.unwrap_or_else(|| "default".to_string());
    
    let proxied_client = state.client_manager.get(proxy.as_deref()).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error(e.to_string()))).into_response()
    })?;
    let noproxy_client = state.client_manager.get(None).map_err(|e| {
         (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error(e.to_string()))).into_response()
    })?;

    let mut instances = match state.click_instances.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error("内部服务错误: Mutex a-poisoned".to_string()))).into_response());
        }
    };
    
    // 修改点 1：在这里克隆 Arc
    let instance = instances
        .entry(session_id)
        .or_insert_with(|| Click::new(Arc::clone(&proxied_client), Arc::clone(&noproxy_client)))
        .clone();

    if proxy.is_some() {
        let mut new_instance = instance;
        // 修改点 2：这里也需要克隆
        new_instance.update_client(Arc::clone(&proxied_client));
        return Ok(new_instance);
    }

    Ok(instance)
}

fn get_slide_instance(
    state: &AppState,
    session_id: Option<String>,
    proxy: Option<String>,
) -> Result<Slide, Response> {
    let session_id = session_id.unwrap_or_else(|| "default".to_string());
    
    let proxied_client = state.client_manager.get(proxy.as_deref()).map_err(|e| {
        (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error(e.to_string()))).into_response()
    })?;
    let noproxy_client = state.client_manager.get(None).map_err(|e| {
         (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error(e.to_string()))).into_response()
    })?;

    let mut instances = match state.slide_instances.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error("内部服务错误: Mutex poisoned".to_string()))).into_response());
        }
    };
    
    // 修改点 1：在这里克隆 Arc
    let instance = instances
        .entry(session_id)
        .or_insert_with(|| Slide::new(Arc::clone(&proxied_client), Arc::clone(&noproxy_client)))
        .clone();
    
    if proxy.is_some() {
        let mut new_instance = instance;
        // 修改点 2：这里也需要克隆
        new_instance.update_client(Arc::clone(&proxied_client));
        return Ok(new_instance);
    }
        
    Ok(instance)
}

// 辅助宏来简化 handler 中的错误处理
macro_rules! handle_blocking_call {
    ($instance_result:expr, $block:expr) => {
        {
            let mut instance = match $instance_result {
                Ok(inst) => inst,
                Err(resp) => return resp,
            };

            match task::spawn_blocking(move || $block(&mut instance)).await {
                Ok(Ok(data)) => Json(ApiResponse::success(data)).into_response(),
                Ok(Err(e)) => (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::error(e.to_string()))).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::error(e.to_string()))).into_response(),
            }
        }
    };
}


// --- Click 相关的处理函数 (修改返回类型) ---
async fn click_simple_match(State(state): State<AppState>, Json(req): Json<SimpleMatchRequest>) -> Response {
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.simple_match(&req.gt, &req.challenge)
    )
}

async fn click_simple_match_retry(State(state): State<AppState>, Json(req): Json<SimpleMatchRequest>) -> Response {
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.simple_match_retry(&req.gt, &req.challenge)
    )
}

async fn click_register_test(State(state): State<AppState>, Json(req): Json<RegisterTestRequest>) -> Response {
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.register_test(&req.url).map(|(f, s)| TupleResponse2 { first: f, second: s })
    )
}

async fn click_get_c_s(State(state): State<AppState>, Json(req): Json<GetCSRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.get_c_s(&req.gt, &req.challenge, w_owned.as_deref()).map(|(c, s)| CSResponse { c, s })
    )
}

async fn click_get_type(State(state): State<AppState>, Json(req): Json<GetTypeRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.get_type(&req.gt, &req.challenge, w_owned.as_deref()).map(|t| match t {
            VerifyType::Click => "click".to_string(),
            VerifyType::Slide => "slide".to_string(),
        })
    )
}

async fn click_verify(State(state): State<AppState>, Json(req): Json<VerifyRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.verify(&req.gt, &req.challenge, w_owned.as_deref()).map(|(f, s)| TupleResponse2 { first: f, second: s })
    )
}

async fn click_generate_w(State(state): State<AppState>, Json(req): Json<GenerateWRequest>) -> Response {
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.generate_w(&req.key, &req.gt, &req.challenge, &req.c, &req.s)
    )
}

async fn click_test(State(state): State<AppState>, Json(req): Json<TestRequest>) -> Response {
    handle_blocking_call!(
        get_click_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Click| instance.test(&req.url)
    )
}

// --- Slide 相关的处理函数 (修改返回类型) ---
async fn slide_register_test(State(state): State<AppState>, Json(req): Json<RegisterTestRequest>) -> Response {
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.register_test(&req.url).map(|(f, s)| TupleResponse2 { first: f, second: s })
    )
}

async fn slide_get_c_s(State(state): State<AppState>, Json(req): Json<GetCSRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.get_c_s(&req.gt, &req.challenge, w_owned.as_deref()).map(|(c, s)| CSResponse { c, s })
    )
}

async fn slide_get_type(State(state): State<AppState>, Json(req): Json<GetTypeRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.get_type(&req.gt, &req.challenge, w_owned.as_deref()).map(|t| match t {
            VerifyType::Click => "click".to_string(),
            VerifyType::Slide => "slide".to_string(),
        })
    )
}

async fn slide_verify(State(state): State<AppState>, Json(req): Json<VerifyRequest>) -> Response {
    let w_owned = req.w.clone();
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.verify(&req.gt, &req.challenge, w_owned.as_deref()).map(|(f, s)| TupleResponse2 { first: f, second: s })
    )
}

async fn slide_generate_w(State(state): State<AppState>, Json(req): Json<GenerateWRequest>) -> Response {
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.generate_w(&req.key, &req.gt, &req.challenge, &req.c, &req.s)
    )
}

async fn slide_test(State(state): State<AppState>, Json(req): Json<TestRequest>) -> Response {
    handle_blocking_call!(
        get_slide_instance(&state, req.session_id, req.proxy),
        move |instance: &mut Slide| instance.test(&req.url)
    )
}


// 健康检查端点
async fn health_check() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() {
    let state = AppState::new();
    
    // ... 路由部分保持不变
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/click/simple_match", post(click_simple_match))
        .route("/click/simple_match_retry", post(click_simple_match_retry))
        .route("/click/register_test", post(click_register_test))
        .route("/click/get_c_s", post(click_get_c_s))
        .route("/click/get_type", post(click_get_type))
        .route("/click/verify", post(click_verify))
        .route("/click/generate_w", post(click_generate_w))
        .route("/click/test", post(click_test))
        .route("/slide/register_test", post(slide_register_test))
        .route("/slide/get_c_s", post(slide_get_c_s))
        .route("/slide/get_type", post(slide_get_type))
        .route("/slide/verify", post(slide_verify))
        .route("/slide/generate_w", post(slide_generate_w))
        .route("/slide/test", post(slide_test))
        .layer(ServiceBuilder::new().layer(CorsLayer::permissive()))
        .with_state(state);

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
        
    println!("🚀 Server starting on http://0.0.0.0:3000");
    println!("📋 Available endpoints:");
    println!("  GET  /health - Health check");
    println!("  POST /click/* - All click operations");
    println!("  POST /slide/* - All slide operations");
    println!("  (All POST endpoints accept optional 'proxy' and 'session_id' fields)");
    
    axum::serve(listener, app).await.unwrap();
}