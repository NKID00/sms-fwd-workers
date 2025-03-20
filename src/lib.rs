#![feature(let_chains)]

use std::{fmt::Display, sync::OnceLock};

use indoc::indoc;
use itertools::Itertools;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;
use worker::{kv::KvStore, *};

const HEARTBEAT_INTERVAL_SECONDS: i64 = 300;

static RE_CODE: OnceLock<Regex> = OnceLock::new();

static COMMAND_MAIL: OnceLock<String> = OnceLock::new();

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
        write!(f, "<code>{sender}</code>\n\n{text}", sender = self.sender())
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

#[derive(Debug, Deserialize)]
struct StatusReport {
    pub battery: i32,
    pub charger: bool,
}

#[derive(Debug, Deserialize)]
struct Update {
    message: Message,
}

impl Update {
    pub fn user_id(&self) -> Option<i64> {
        self.message.from.as_ref().map(|user| user.id)
    }

    pub fn chat_id(&self) -> i64 {
        self.message.chat.id
    }

    pub fn text(&self) -> &str {
        &self.message.text
    }
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

#[derive(Debug, Deserialize)]
struct MessageResponse {
    ok: bool,
    result: Option<Message>,
}

impl MessageResponse {
    pub fn ok(&self) -> bool {
        self.ok
    }

    pub fn message_id(&self) -> i64 {
        self.result.as_ref().unwrap().message_id
    }

    pub fn chat_id(&self) -> i64 {
        self.result.as_ref().unwrap().chat.id
    }
}

impl Display for MessageResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.ok() {
            write!(f, "sent {} to {}", self.message_id(), self.chat_id())
        } else {
            write!(f, "failed")
        }
    }
}

#[derive(Debug, Serialize)]
struct EditMessageTextBody<'a> {
    chat_id: i64,
    message_id: i64,
    text: &'a str,
    parse_mode: &'a str,
}

#[derive(Debug, Deserialize)]
struct Message {
    message_id: i64,
    from: Option<User>,
    chat: Chat,
    text: String,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn timestamp_ms() -> i64 {
    Date::now().as_millis() as i64
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

fn get_bot_token(env: &Env) -> String {
    get_secret(env, "bot_token")
}

fn get_chat_id(env: &Env, device: &str) -> String {
    get_secret(env, &format!("{device}_chat_id"))
}

fn from_json<T: DeserializeOwned>(s: &str) -> Option<T> {
    T::deserialize(serde_wasm_bindgen::Deserializer::from(
        js_sys::JSON::parse(s).ok()?,
    ))
    .ok()
}

fn to_json<T: Serialize>(v: T) -> String {
    const SERIALIZER: serde_wasm_bindgen::Serializer =
        serde_wasm_bindgen::Serializer::json_compatible();
    js_sys::JSON::stringify(&v.serialize(&SERIALIZER).unwrap())
        .unwrap()
        .into()
}

async fn send_message(env: &Env, body: &SendMessageBody<'_>) -> Option<i64> {
    let bot_token = get_bot_token(env);
    let body = to_json(body);
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
        Ok(mut response) => {
            let Ok(response) = response.json::<MessageResponse>().await else {
                console_error!("sendMessage invalid response: {response:?}");
                return None;
            };
            console_log!("sendMessage: {response}");
            Some(response.message_id())
        }
        Err(e) => {
            console_error!("sendMessage failed: {e:?}");
            None
        }
    }
}

async fn send_message_by_chat(env: &Env, chat_id: i64, text: &str) -> Option<i64> {
    send_message(
        env,
        &SendMessageBody {
            chat_id: &chat_id.to_string(),
            text,
            parse_mode: "HTML",
        },
    )
    .await
}

async fn send_message_by_device(env: &Env, device: &str, text: &str) -> Option<i64> {
    send_message(
        env,
        &SendMessageBody {
            chat_id: &get_chat_id(env, device),
            text,
            parse_mode: "HTML",
        },
    )
    .await
}

async fn send_sticker(env: &Env, device: &str, sticker: &str) {
    let bot_token = get_bot_token(&env);
    let chat_id = get_secret(env, &format!("{device}_chat_id"));
    let body = to_json(&SendStickerBody {
        chat_id: &chat_id.to_string(),
        sticker,
    });
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
        Ok(mut response) => {
            let Ok(response) = response.json::<MessageResponse>().await else {
                console_error!("sendSticker invalid response: {response:?}");
                return;
            };
            console_log!("sendSticker: {response}")
        }
        Err(e) => console_error!("sendSticker failed: {e:?}"),
    };
}

#[wasm_bindgen(module = "cloudflare:email")]
extern "C" {
    #[wasm_bindgen(extends=js_sys::Object)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    type EmailMessage;

    #[wasm_bindgen(constructor, catch)]
    fn new(from: String, to: String, raw: String) -> Result<EmailMessage>;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends=js_sys::Object)]
    #[derive(Debug, Clone, PartialEq, Eq)]
    type SendEmail;

    #[wasm_bindgen(method, catch)]
    async fn send(this: &SendEmail, message: EmailMessage) -> Result<()>;
}

impl EnvBinding for SendEmail {
    const TYPE_NAME: &'static str = "SendEmail";
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name=randomUUID, js_namespace=crypto)]
    fn random_uuid() -> String;
}

async fn send_email(env: &Env, device: &str) -> Result<()> {
    let from = get_secret(env, &format!("{device}_mail_from"));
    let to = get_secret(env, &format!("{device}_mail_to"));
    let ts = timestamp_ms();
    let id = format!(
        "{ts}.{uuid}@{domain}",
        uuid = random_uuid(),
        domain = from.rsplit_once("@").unwrap().1
    );
    let raw = COMMAND_MAIL
        .get()
        .unwrap()
        .replace("{{from}}", &from)
        .replace("{{to}}", &to)
        .replace("{{id}}", &id)
        .replace("{{device}}", device);
    let mail = EmailMessage::new(from, to.clone(), raw).unwrap();
    let command: SendEmail = env.get_binding("command").unwrap();
    let result = command.send(mail).await;
    match &result {
        Ok(()) => console_log!("sendEmail: sent {id} to {to}"),
        Err(e) => console_log!("sendEmail failed: {e:?}"),
    }
    result
}

async fn edit_message(env: &Env, body: &EditMessageTextBody<'_>) {
    let bot_token = get_bot_token(env);
    let body = to_json(body);
    let request = Request::new_with_init(
        &format!("https://api.telegram.org/bot{bot_token}/editMessageText"),
        &RequestInit {
            method: Method::Post,
            headers: [("Content-Type", "application/json")].into_iter().collect(),
            body: Some(body.into()),
            ..RequestInit::default()
        },
    )
    .unwrap();
    match Fetch::Request(request).send().await {
        Ok(mut response) => {
            let Ok(response) = response.json::<MessageResponse>().await else {
                console_error!("editMessageText invalid response: {response:?}");
                return;
            };
            console_log!("editMessageText: {response}")
        }
        Err(e) => console_error!("editMessageText failed: {e:?}"),
    };
}

async fn edit_message_by_chat(env: &Env, chat_id: i64, message_id: i64, text: &str) {
    edit_message(
        env,
        &EditMessageTextBody {
            chat_id,
            message_id,
            text,
            parse_mode: "HTML",
        },
    )
    .await
}

fn check_token(device: &str, token: &str, env: &Env) -> bool {
    let Ok(secret) = env.secret(device) else {
        return false;
    };
    token == secret.to_string()
}

async fn authorize(req: &mut Request, env: &Env) -> Option<AuthorizedRequest> {
    if !matches!(req.method(), Method::Get | Method::Post) {
        return None;
    }
    let authorization = match req.headers().get("Authorization").unwrap() {
        Some(s) => s.trim().trim_start_matches("Bearer ").to_owned(),
        None => {
            let path = req
                .path()
                .trim_start_matches("/")
                .trim_end_matches("/")
                .to_owned();
            if path.is_empty() {
                if req.method() == Method::Post
                    && let Some(s) = req
                        .headers()
                        .get("X-Telegram-Bot-Api-Secret-Token")
                        .unwrap()
                    && s == get_secret(env, "update_secret")
                    && let Ok(update) = req.json().await
                {
                    return Some(AuthorizedRequest::MessageUpdate { update });
                } else {
                    return None;
                }
            } else {
                path
            }
        }
    };
    let (device, token) = authorization
        .splitn(2, '/')
        .map(ToOwned::to_owned)
        .collect_tuple()?;
    if !check_token(&device, &token, env) {
        return None;
    }
    match req.method() {
        Method::Get => Some(AuthorizedRequest::GetConfig { device, token }),
        Method::Post => {
            let body = req.text().await.ok()?;
            if body.is_empty() {
                Some(AuthorizedRequest::Heartbeat { device })
            } else if let Some(query) = from_json(&body) {
                Some(AuthorizedRequest::Forward { device, query })
            } else if let Some(status) = from_json(&body) {
                Some(AuthorizedRequest::ReportStatus { device, status })
            } else {
                Some(AuthorizedRequest::Unknown { device, body })
            }
        }
        _ => None,
    }
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

async fn forward(device: String, query: AppleMessageFilterQuery, env: Env) {
    send_message_by_device(&env, &device, &format!("{device} {query}")).await;
}

async fn heartbeat(device: String, env: Env) {
    let kv = env.kv("sms-forward-heartbeat").unwrap();
    let status = HeartbeatStatus::get(&kv, &device).await;
    console_log!("refresh {device}, previous {status:?}");
    if status != Active {
        send_message_by_device(&env, &device, &format!("🟢 {device} is now up")).await;
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

async fn report_status(device: String, status: StatusReport, env: Env) {
    send_message_by_device(
        &env,
        &device,
        &format!(
            "{emoji} {device} {battery:?}% {charger}",
            emoji = if status.charger { "⚡️" } else { "🔋" },
            battery = status.battery,
            charger = if status.charger {
                "charging"
            } else {
                "discharging"
            }
        ),
    )
    .await;
}

async fn message_update(update: Update, env: Env) {
    let Some(user_id) = update.user_id() else {
        return;
    };
    let trusted_chat_ids = get_secret(&env, "trusted_chat_ids")
        .split(',')
        .filter_map(|s| s.parse::<i64>().ok())
        .collect_vec();
    if !trusted_chat_ids.contains(&update.chat_id()) {
        return;
    }
    let trusted_user_ids = get_secret(&env, "trusted_user_ids")
        .split(',')
        .filter_map(|s| s.parse::<i64>().ok())
        .collect_vec();
    if (!trusted_user_ids.is_empty()) && (!trusted_user_ids.contains(&user_id)) {
        return;
    }

    let mut args = update.text().split_whitespace();
    let Some(command) = args.next() else {
        return;
    };
    if command.starts_with("/version@") || command == "/version" {
        console_log!("answer version");
        let version: WorkerVersionMetadata = env.get_binding("version").unwrap();
        send_message_by_chat(
            &env,
            update.chat_id(),
            &format!("<code>{}</code> at {}", version.id(), version.timestamp()),
        )
        .await;
    } else if command.starts_with("/info@") || command == "/info" {
        let Some(device) = args.next() else {
            send_message_by_chat(&env, update.chat_id(), "Argument &lt;device&gt; required").await;
            return;
        };
        if !get_secret(&env, "devices").split(',').contains(&device) {
            send_message_by_chat(&env, update.chat_id(), "Device not found").await;
            return;
        }
        if env.secret(&format!("{device}_mail_to")).is_err() {
            send_message_by_chat(&env, update.chat_id(), "Device email not configured").await;
            return;
        }
        console_log!("command {device}");
        let Some(message_id) =
            send_message_by_chat(&env, update.chat_id(), "Sending command").await
        else {
            return;
        };
        match send_email(&env, device).await {
            Ok(()) => {
                edit_message_by_chat(&env, update.chat_id(), message_id, "Command sent").await
            }
            Err(e) => {
                console_error!("sendEmail failed: {e:?}");
                edit_message_by_chat(&env, update.chat_id(), message_id, "failed to send command")
                    .await
            }
        };
    }
}

async fn echo(device: String, body: String, env: Env) {
    let text = format!("{}\n\n<pre>{}</pre>", device, escape_html(&body));
    send_message_by_device(&env, &device, &text).await;
}

#[derive(Debug)]
enum AuthorizedRequest {
    GetConfig {
        device: String,
        token: String,
    },
    Forward {
        device: String,
        query: AppleMessageFilterQuery,
    },
    Heartbeat {
        device: String,
    },
    ReportStatus {
        device: String,
        status: StatusReport,
    },
    MessageUpdate {
        update: Update,
    },
    Unknown {
        device: String,
        body: String,
    },
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    let Some(request) = authorize(&mut req, &env).await else {
        return Response::empty();
    };
    match request {
        AuthorizedRequest::GetConfig { device, token } => generate_config(device, token, env).await,
        AuthorizedRequest::Forward { device, query } => {
            ctx.wait_until(heartbeat(device.clone(), env.clone()));
            ctx.wait_until(forward(device, query, env));
            Response::empty()
        }
        AuthorizedRequest::Heartbeat { device } => {
            ctx.wait_until(heartbeat(device, env));
            Response::empty()
        }
        AuthorizedRequest::ReportStatus { device, status } => {
            ctx.wait_until(heartbeat(device.clone(), env.clone()));
            ctx.wait_until(report_status(device, status, env));
            Response::empty()
        }
        AuthorizedRequest::MessageUpdate { update } => {
            ctx.wait_until(message_update(update, env));
            Response::empty()
        }
        AuthorizedRequest::Unknown { device, body } => {
            ctx.wait_until(echo(device, body, env));
            Response::empty()
        }
    }
}

#[allow(unused)]
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let kv = env.kv("sms-forward-heartbeat").unwrap();
    for device in get_secret(&env, "devices").split(",") {
        let status = HeartbeatStatus::get(&kv, device).await;
        console_log!("check {device}, previous {status:?}");
        if status == Inactive {
            send_message_by_device(&env, device, &format!("🔴 {device} is DOWN ⚠️")).await;
            send_sticker(&env, device, &get_secret(&env, "down_sticker")).await;
        }
    }
}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
    RE_CODE.get_or_init(|| Regex::new(r"(?:[[:alnum:]]-)?[[:digit:]]{6}").unwrap());
    COMMAND_MAIL.get_or_init(|| {
        indoc! {r#"
        From: "Remote Command" <{{from}}>
        To: "{{device}}" <{{to}}>
        Message-ID: <{{id}}>
        Subject: Command to report status, {{device}}
        MIME-Version: 1.0
        Content-Type: text/plain; charset="utf-8"

        Report status, {{device}}.
    "#}
        .replace("\n", "\r\n")
    });
    console_debug!("{}", COMMAND_MAIL.get().unwrap());
}
