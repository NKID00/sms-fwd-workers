#![feature(let_chains)]

use std::{fmt::Display, sync::OnceLock};

use itertools::Itertools;
use js_sys::Date;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use worker::{kv::KvStore, *};

const HEARTBEAT_INTERVAL_SECONDS: i64 = 300;

static RE_CODE: OnceLock<Regex> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct AppleMessageFilterQuery {
    #[serde(rename = "query")]
    inner: AppleMessageFilterQueryInner,
}

impl AppleMessageFilterQuery {
    fn sender(&self) -> &str {
        &self.inner.sender
    }

    fn text(&self) -> &str {
        &self.inner.message.text
    }
}

impl Display for AppleMessageFilterQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let escaped = escape_html(self.text());
        let text = RE_CODE
            .get()
            .unwrap()
            .replace_all(&escaped, |c: &Captures| {
                format!(" 👉 <code>{}</code> 👈 ", c.get(0).unwrap().as_str())
            });
        write!(f, "<code>{}</code>\n\n{}", self.sender(), text)
    }
}

#[derive(Debug, Deserialize)]
struct AppleMessageFilterQueryInner {
    sender: String,
    message: AppleMessageFilterQueryMessage,
}

#[derive(Debug, Deserialize)]
struct AppleMessageFilterQueryMessage {
    text: String,
}

#[derive(Debug, Serialize)]
struct SendMessageBody<'a> {
    chat_id: &'a str,
    text: &'a str,
    parse_mode: &'a str,
}

#[derive(Debug, Serialize)]
struct SendStickerBody<'a> {
    chat_id: &'a str,
    sticker: &'a str,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn timestamp_ms() -> i64 {
    Date::new_0().get_time() as i64
}

fn get_secret(env: &Env, key: &str) -> String {
    env.secret(key)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| panic!("secret {key} not found"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeartbeatStatus {
    Active,   // ..(interval * 1.5)
    Inactive, // (interval * 1.5)..(interval * 2.5)
    Dead,     // (interval * 2.5)..
}

use HeartbeatStatus::*;

impl HeartbeatStatus {
    async fn get(kv: &KvStore, device: &str) -> Self {
        let result = kv.get(device).text().await.expect("failed to access kv");
        if let Some(v) = result
            && let Ok(previous_timestamp_ms) = v.parse::<i64>()
        {
            let interval = timestamp_ms() - previous_timestamp_ms;
            if interval < HEARTBEAT_INTERVAL_SECONDS * 1500 {
                return Active;
            } else if interval < HEARTBEAT_INTERVAL_SECONDS * 2500 {
                return Inactive;
            }
        }
        Dead
    }
}

fn get_chat_info(env: &Env, device: &str) -> (String, String) {
    (
        get_secret(env, &format!("{device}_bot_token")),
        get_secret(env, &format!("{device}_chat_id")),
    )
}

async fn send_message(env: &Env, device: &str, text: &str) {
    let (bot_token, chat_id) = get_chat_info(env, device);
    let body = serde_json::to_string(&SendMessageBody {
        chat_id: &chat_id.to_string(),
        text,
        parse_mode: "HTML",
    })
    .unwrap();
    let request = Request::new_with_init(
        &format!("https://api.telegram.org/bot{bot_token}/sendMessage"),
        &RequestInit {
            method: Method::Post,
            headers: [("Content-Type", "application/json")].into_iter().collect(),
            body: Some(body.into()),
            ..RequestInit::default()
        },
    )
    .unwrap();
    match Fetch::Request(request).send().await {
        Ok(response) => console_debug!("{response:?}"),
        Err(e) => console_error!("sendMessage failed: {e:?}"),
    };
}

async fn send_sticker(env: &Env, device: &str, sticker: &str) {
    let bot_token = get_secret(env, &format!("{device}_bot_token"));
    let chat_id = get_secret(env, &format!("{device}_chat_id"));
    let body = serde_json::to_string(&SendStickerBody {
        chat_id: &chat_id.to_string(),
        sticker,
    })
    .unwrap();
    let request = Request::new_with_init(
        &format!("https://api.telegram.org/bot{bot_token}/sendSticker"),
        &RequestInit {
            method: Method::Post,
            headers: [("Content-Type", "application/json")].into_iter().collect(),
            body: Some(body.into()),
            ..RequestInit::default()
        },
    )
    .unwrap();
    match Fetch::Request(request).send().await {
        Ok(response) => console_debug!("{response:?}"),
        Err(e) => console_error!("sendSticker failed: {e:?}"),
    };
}

fn authorize(req: &Request, env: &Env) -> Option<(String, String)> {
    if !matches!(req.method(), Method::Get | Method::Post) {
        return None;
    }
    let authorization = match req.headers().get("Authorization").unwrap() {
        Some(s) => s.trim().trim_start_matches("Bearer ").to_owned(),
        None => req
            .path()
            .trim_start_matches("/")
            .trim_end_matches("/")
            .to_owned(),
    };
    let (device, token) = authorization.splitn(2, '/').collect_tuple()?;
    let Ok(secret) = env.secret(device) else {
        return None;
    };
    if token != secret.to_string() {
        return None;
    }
    Some((device.to_owned(), token.to_owned()))
}

async fn generate_config(device: String, token: String, env: Env) -> Result<Response> {
    let url = get_secret(&env, "config_template_url");
    let request = Request::new(&url, Method::Get)?;
    let template = Fetch::Request(request).send().await?.text().await?;
    let body = template
        .replace("{{token}}", &format!("{device}/{token}"))
        .into_bytes();
    Ok(Response::builder()
        .with_headers(
            [
                ("Content-Type", "text/plain; charset=utf-8"),
                (
                    "Content-Disposition",
                    "inline; filename=\"sms-forward.yaml\"",
                ),
            ]
            .into_iter()
            .collect(),
        )
        .fixed(body))
}

async fn heartbeat(device: String, env: Env) {
    let kv = env.kv("sms-forward-heartbeat").unwrap();
    let status = HeartbeatStatus::get(&kv, &device).await;
    console_debug!("heartbeat for {device}, previous {status:?}");
    if status != Active {
        send_message(&env, &device, &format!("🟢 {device} is now up")).await;
        send_sticker(&env, &device, &get_secret(&env, "up_sticker")).await;
    }
    if let Err(e) = kv
        .put(&device, timestamp_ms())
        .unwrap()
        .expiration_ttl((HEARTBEAT_INTERVAL_SECONDS as f64 * 2.5) as u64)
        .execute()
        .await
    {
        console_error!("failed to put kv for key {device:?}: {e:?}");
    };
}

async fn forward(device: String, body: Vec<u8>, env: Env) {
    let text = match serde_json::from_slice::<AppleMessageFilterQuery>(&body) {
        Ok(message) => {
            format!("{device} {message}")
        }
        Err(_) => {
            format!(
                "{}\n\n<pre>{}</pre>",
                device,
                escape_html(&String::from_utf8_lossy(&body)),
            )
        }
    };
    send_message(&env, &device, &text).await;
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    let Some((device, token)) = authorize(&req, &env) else {
        return Response::empty();
    };
    if req.method() == Method::Get {
        generate_config(device, token, env).await
    } else {
        ctx.wait_until(heartbeat(device.clone(), env.clone()));
        let body = req.bytes().await.unwrap();
        if !body.is_empty() {
            ctx.wait_until(forward(device, body, env));
        }
        Response::empty()
    }
}

#[allow(unused)]
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let kv = env.kv("sms-forward-heartbeat").unwrap();
    for device in get_secret(&env, "devices").split(",") {
        let status = HeartbeatStatus::get(&kv, device).await;
        console_debug!("scheduled check for {device}, previous {status:?}");
        if status == Inactive {
            send_message(&env, device, &format!("🔴 {device} is DOWN‼ ⚠️")).await;
            send_sticker(&env, device, &get_secret(&env, "down_sticker")).await;
        }
    }
}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
    RE_CODE.get_or_init(|| Regex::new(r"(?:[[:alnum:]]-)?[[:digit:]]{6}").unwrap());
}
