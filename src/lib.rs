#![feature(let_chains)]

use std::sync::OnceLock;

use itertools::Itertools;
use regex::Regex;
use serde::{Deserialize, Serialize};
use worker::*;

static RE_SMS_HAS_CODE: OnceLock<Regex> = OnceLock::new();
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

    fn code(&self) -> Option<&str> {
        if !RE_SMS_HAS_CODE.get().unwrap().is_match(self.text()) {
            return None;
        }
        RE_CODE
            .get()
            .unwrap()
            .captures(self.text())?
            .get(1)
            .map(|m| m.as_str())
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

fn authorize(req: &Request, env: &Env) -> Option<String> {
    if req.method() != Method::Post {
        return None;
    }
    let authorization = req.headers().get("Authorization").unwrap()?;
    let authorization = authorization.trim().trim_start_matches("Bearer ");
    let (device, token) = authorization.splitn(2, '/').collect_tuple()?;
    let Ok(secret) = env.secret(device) else {
        return None;
    };
    if token != secret.to_string() {
        return None;
    }
    Some(device.to_string())
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    if let Some(device) = authorize(&req, &env)
        && let Ok(body) = req.bytes().await
    {
        ctx.wait_until(forward(device, body, env));
    }
    Response::empty()
}

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
        Ok(query) => match query.code() {
            Some(code) => format!(
                "{} <code>{}</code> <b>[<code>{}</code>]</b>\n\n{}",
                device,
                escape_html(query.sender()),
                code,
                escape_html(query.text())
            ),
            None => format!(
                "{} <code>{}</code>\n\n{}",
                device,
                escape_html(query.sender()),
                escape_html(query.text())
            ),
        },
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
            headers: [("content-type", "application/json")].into_iter().collect(),
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

#[allow(unused)]
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {}

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
    RE_SMS_HAS_CODE.get_or_init(|| {
        Regex::new(
            r"验证码|校验码|交易码|[Cc](?:ODE|ode)|[Vv](?:ERIFY|erify|ERIFICATION|erification)",
        )
        .unwrap()
    });
    RE_CODE.get_or_init(|| {
        Regex::new(r"(?:^|[[:^digit:]])((?:[[:alnum:]]-)?[[:digit:]]{4,8})(?:$|[[:^digit:]])")
            .unwrap()
    });
}
