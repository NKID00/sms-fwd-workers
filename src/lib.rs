#![feature(let_chains)]

use std::{fmt::Display, sync::OnceLock};

use itertools::Itertools;
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use worker::*;

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
struct SendMessageBody {
    chat_id: String,
    text: String,
    parse_mode: String,
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

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn generate_config(device: String, token: String, env: Env) -> Result<String> {
    let url = env.secret("config_template_url")?.to_string();
    let request = Request::new(&url, Method::Get)?;
    let template = Fetch::Request(request).send().await?.text().await?;
    Ok(template.replace("{{token}}", &format!("{device}/{token}")))
}

async fn heartbeat(device: String, env: Env) {}

async fn forward(device: String, body: Vec<u8>, env: Env) {
    let Ok(bot_token) = env.secret(&format!("{device}_bot_token")) else {
        console_error!("bot_token not found for {device:?}");
        return;
    };
    let Ok(chat_id) = env.secret(&format!("{device}_chat_id")) else {
        console_error!("chat_id not found for {device:?}");
        return;
    };
    let text = match serde_json::from_slice::<AppleMessageFilterQuery>(&body) {
        Ok(query) => {
            format!("{device} {query}")
        }
        Err(_) => {
            format!(
                "{}\n\n<pre>{}</pre>",
                device,
                escape_html(&String::from_utf8_lossy(&body)),
            )
        }
    };
    let body = serde_json::to_string(&SendMessageBody {
        chat_id: chat_id.to_string(),
        text,
        parse_mode: "HTML".to_string(),
    })
    .unwrap();
    let Ok(request) = Request::new_with_init(
        &format!("https://api.telegram.org/bot{bot_token}/sendMessage"),
        &RequestInit {
            method: Method::Post,
            headers: [("Content-Type", "application/json")].into_iter().collect(),
            body: Some(body.into()),
            ..RequestInit::default()
        },
    ) else {
        console_error!("sendMessage request construct failed");
        return;
    };
    match Fetch::Request(request).send().await {
        Ok(response) => console_log!("{response:?}"),
        Err(e) => console_error!("sendMessage failed: {e:?}"),
    };
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    let Some((device, token)) = authorize(&req, &env) else {
        return Response::empty();
    };
    if req.method() == Method::Get {
        Ok(Response::builder()
            .with_headers(
                [
                    ("Content-Type", "text/plain; charset=utf-8"),
                    ("Content-Disposition", "inline; filename=\"sms-forward.yaml\""),
                ]
                .into_iter()
                .collect(),
            )
            .fixed(
                generate_config(device, token, env)
                    .await
                    .unwrap_or_default()
                    .into_bytes(),
            ))
    } else {
        let body = req.bytes().await.unwrap();
        if body.is_empty() {
            ctx.wait_until(heartbeat(device, env));
        } else {
            ctx.wait_until(forward(device, body, env));
        }
        Response::empty()
    }
}

#[allow(unused)]
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
    RE_CODE.get_or_init(|| Regex::new(r"(?:[[:alnum:]]-)?[[:digit:]]{6}").unwrap());
}
