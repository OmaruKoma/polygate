//! Signed inference gateway tests (M13-D2-C). No test opens a real upstream.

use super::*;
use crate::accounts::Credential;
use crate::config::{Region, Verification};
use crate::providers::{ChatStream, Provider};
use crate::test_support::{spawn_server, Recorded, Reply};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::StreamExt;

use super::super::gateway::parse_models;
use super::super::provider::QoderProvider;

const MODELS_PATH: &str = "/algo/api/v2/model/list";
const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";

fn provider(base: &str) -> QoderProvider {
    QoderProvider::with_base(
        "qoder-cn",
        Region::Cn,
        Verification::Experimental,
        reqwest::Client::new(),
        base,
    )
}

fn credential() -> Credential {
    Credential::from_value(json!({
        "access_token": "dt-access",
        "refresh_token": "drt-refresh",
        "uid": "uid-1",
        "nickname": "N",
        "user_type": "personal_pro",
        "organization_id": "org-1",
        "organization_name": "Org",
        "realm": "cn",
        "expires_at": 0,
    }))
}

/// Recompute the COSY digest from the wire values and check the encrypted
/// payload envelope.
fn assert_signed(request: &Recorded, signed_body: &str) {
    let authorization = request
        .header("authorization")
        .expect("signed request has an authorization header");
    let rest = authorization
        .strip_prefix("Bearer COSY.")
        .expect("COSY authorization scheme");
    let (payload, digest) = rest.split_once('.').expect("payload.digest");
    let key = request.header("cosy-key").expect("cosy-key");
    let date = request.header("cosy-date").expect("cosy-date");
    let url = format!("http://mock{}", request.target);
    let path = cosy::sign_path(&url).unwrap();
    assert_eq!(
        cosy::cosy_signature(payload, key, date, signed_body, &path),
        digest,
        "digest must cover the exact body that was sent"
    );

    let decoded = BASE64.decode(payload).expect("standard base64 payload");
    let envelope: Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(envelope["version"], "v1");
    assert_eq!(envelope["cosyVersion"], cosy::COSY_VERSION);
    assert_eq!(envelope["ideVersion"], "");
    let request_id = envelope["requestId"].as_str().expect("request id");
    assert_eq!(request_id.len(), 32, "requestId is a uuid without dashes");
    assert!(request_id.chars().all(|ch| ch.is_ascii_hexdigit()));
    assert!(envelope["info"]
        .as_str()
        .is_some_and(|info| !info.is_empty()));
    assert_eq!(request.header("cosy-user"), Some("uid-1"));
    assert!(request.header("cosy-key").is_some());
    assert!(request.header("cosy-date").is_some());
    assert_eq!(request.header("user-agent"), Some("Go-http-client/2.0"));
    assert!(
        request.header("cosy-clienttype").is_none(),
        "client-identity headers are not part of the verified protocol"
    );
}

async fn collect(mut stream: ChatStream) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    while let Some(item) = stream.next().await {
        events.push(item.expect("stream item"));
    }
    events
}

#[tokio::test]
async fn model_list_is_signed_and_parsed() {
    let server = spawn_server(|request| match request.path() {
        MODELS_PATH => Reply::json(
            200,
            json!({
                "chat": [
                    {
                        "key": "qmodel_38max",
                        "display_name": "Qwen 3.8 Max",
                        "max_input_tokens": 262144,
                        "supportsToolCall": true
                    },
                    {"display_name": "no key"},
                    {"key": "  "},
                    {"key": "disabled_model", "display_name": "Off", "enable": false}
                ]
            })
            .to_string(),
        ),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let models = provider(&server.base_url)
        .list_models(&credential())
        .await
        .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "qmodel_38max");
    assert_eq!(models[0].display_name.as_deref(), Some("Qwen 3.8 Max"));
    assert_eq!(models[0].context_window, Some(262_144));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].target, format!("{MODELS_PATH}?Encode=1"));
    assert_eq!(requests[0].body, "", "a GET signs the empty body");
    assert!(requests[0].header("x-model-key").is_none());
    assert_signed(&requests[0], "");
}

#[test]
fn an_empty_or_missing_catalogue_is_a_discovery_failure() {
    let empty = parse_models(&json!({"chat": []})).unwrap_err();
    assert_eq!(empty.kind, ErrorKind::UpstreamServer);
    let missing = parse_models(&json!({"data": []})).unwrap_err();
    assert_eq!(missing.kind, ErrorKind::UpstreamServer);
    let unnamed = parse_models(&json!({"chat": [{"display_name": "x"}]})).unwrap_err();
    assert_eq!(unnamed.kind, ErrorKind::UpstreamServer);
}

#[tokio::test]
async fn chat_stream_translates_frames_and_sends_the_encoded_body() {
    let server = spawn_server(|request| match request.path() {
        CHAT_PATH => Reply::sse(CHAT_SSE),
        _ => Reply::json(404, "{}"),
    })
    .await;

    let events = collect(
        provider(&server.base_url)
            .chat_stream(&credential(), basic_request())
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(text_of(&events), "Hello ");
    assert_eq!(reasoning_of(&events), "think ");
    assert_eq!(tool_args_of(&events), "{\"city\":\"Beijing\"}");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Finish { reason } if *reason == FinishReason::ToolCalls
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Usage { usage } if usage.input_tokens == 10 && usage.output_tokens == 5
    )));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].target,
        format!("{CHAT_PATH}?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1")
    );
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert_eq!(requests[0].header("accept"), Some("text/event-stream"));
    assert_eq!(requests[0].header("cache-control"), Some("no-cache"));
    assert_eq!(requests[0].header("x-model-key"), Some("qmodel"));
    assert_eq!(requests[0].header("x-model-source"), Some("system"));

    let decoded = cosy::qoder_decode(&requests[0].body).unwrap();
    let sent: Value = serde_json::from_slice(&decoded).unwrap();
    let expected = convert::build_body(&basic_request()).unwrap();
    assert_eq!(sent, expected);
    assert_eq!(sent["stream"], json!(true));
    assert_eq!(sent["agent_id"], json!("agent_common"));
    assert_signed(&requests[0], &requests[0].body);
}

#[tokio::test]
async fn chat_aggregates_the_stream_into_one_response() {
    let server = spawn_server(|_| Reply::sse(CHAT_SSE)).await;
    let response = provider(&server.base_url)
        .chat(&credential(), basic_request())
        .await
        .unwrap();

    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let text: String = response
        .message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello ");
    let tool_calls: Vec<&ToolCall> = response.message.tool_calls().collect();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].name, "get_weather");
    assert_eq!(tool_calls[0].arguments, "{\"city\":\"Beijing\"}");
    assert_eq!(response.usage.map(|usage| usage.input_tokens), Some(10));
}

#[tokio::test]
async fn chat_maps_quota_rejections_without_touching_the_account() {
    let server =
        spawn_server(|_| Reply::json(402, r#"{"message":"Billing daily count exceeded"}"#)).await;
    let provider = provider(&server.base_url);

    let error = provider
        .chat(&credential(), basic_request())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::QuotaExhausted);

    let stream_error = provider
        .chat_stream(&credential(), basic_request())
        .await
        .err()
        .expect("a rejected chat must not open a stream");
    assert_eq!(stream_error.kind, ErrorKind::QuotaExhausted);
}

#[tokio::test]
async fn a_stream_without_done_or_finish_reports_truncation() {
    let frame =
        "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"}}]}\"}\n\n";
    let server = spawn_server(move |_| Reply::sse(frame)).await;
    let mut stream = provider(&server.base_url)
        .chat_stream(&credential(), basic_request())
        .await
        .unwrap();
    let mut saw_error = false;
    while let Some(item) = stream.next().await {
        if let StreamEvent::Error { error } = item.expect("stream item") {
            assert_eq!(error.kind, ErrorKind::UpstreamServer);
            saw_error = true;
        }
    }
    assert!(
        saw_error,
        "EOF without [DONE] and finish_reason must emit a terminal error"
    );
}

const CONTENT_FRAME: &str = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"}}]}\"}

";

#[tokio::test]
async fn a_quota_sentinel_after_content_terminates_the_stream() {
    let body = format!(
        "{CONTENT_FRAME}data: [EXCEED_QUOTA]

"
    );
    let server = spawn_server(move |_| Reply::sse(body.clone())).await;

    let mut stream = provider(&server.base_url)
        .chat_stream(&credential(), basic_request())
        .await
        .unwrap();
    let mut text = String::new();
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::TextDelta { text: part }) => text.push_str(&part),
            Ok(_) => {}
            Err(error) => {
                terminal = Some(error);
                break;
            }
        }
    }
    assert_eq!(text, "Hi");
    assert_eq!(
        terminal.map(|error| error.kind),
        Some(ErrorKind::QuotaExhausted),
        "a stream cut by [EXCEED_QUOTA] must surface as quota, not truncation"
    );

    // The same body through the non-streaming path fails instead of returning
    // a half answer.
    let server = spawn_server(|_| {
        Reply::sse(format!(
            "{CONTENT_FRAME}data: [EXCEED_QUOTA]

"
        ))
    })
    .await;
    let error = provider(&server.base_url)
        .chat(&credential(), basic_request())
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::QuotaExhausted);
}

#[tokio::test]
async fn an_idle_stream_times_out_instead_of_hanging() {
    let first = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"}}]}\"}

";
    let body = format!(
        "{first}data: {{\"statusCodeValue\":200,\"body\":\"{{\\\"choices\\\":[{{\\\"delta\\\":{{\\\"content\\\":\\\"late\\\"}}}}]}}\"}}

"
    );
    let split = first.len();
    let server = spawn_server(move |_| {
        Reply::sse_stalled(body.clone(), split, std::time::Duration::from_millis(800))
    })
    .await;

    let provider =
        provider(&server.base_url).with_stream_idle_timeout(std::time::Duration::from_millis(150));
    let mut stream = provider
        .chat_stream(&credential(), basic_request())
        .await
        .unwrap();
    let mut text = String::new();
    let mut terminal = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(StreamEvent::TextDelta { text: part }) => text.push_str(&part),
            Ok(_) => {}
            Err(error) => {
                terminal = Some(error);
                break;
            }
        }
    }
    assert_eq!(text, "Hi");
    assert_eq!(terminal.map(|error| error.kind), Some(ErrorKind::Timeout));
}

#[tokio::test]
async fn a_finish_reason_without_done_ends_cleanly() {
    let body = "data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"Hi\\\"},\\\"finish_reason\\\":\\\"stop\\\"}]}\"}

";
    let server = spawn_server(move |_| Reply::sse(body)).await;
    let events = collect(
        provider(&server.base_url)
            .chat_stream(&credential(), basic_request())
            .await
            .unwrap(),
    )
    .await;
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Finish { reason } if *reason == FinishReason::Stop
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, StreamEvent::Error { .. })));
}

#[tokio::test]
async fn impersonation_adds_the_desktop_device_family() {
    let server = spawn_server(|_| Reply::sse(CHAT_SSE)).await;
    let events = collect(
        provider(&server.base_url)
            .with_identity(crate::providers::profile::Identity::Impersonate)
            .chat_stream(&credential(), basic_request())
            .await
            .unwrap(),
    )
    .await;
    assert!(!events.is_empty());

    let request = &server.requests()[0];
    assert_eq!(request.header("cosy-clienttype"), Some("5"));
    assert_eq!(request.header("cosy-data-policy"), Some("AGREE"));
    assert_eq!(request.header("cosy-clientip"), Some("169.254.198.161"));
    // Device identity is derived from the uid, so two accounts never share it.
    assert_eq!(
        request.header("cosy-machineid"),
        Some(cosy::derive_id("uid-1", "machine").as_str())
    );
    assert_eq!(request.header("cosy-machinetype").map(str::len), Some(18));
    assert_eq!(request.header("cosy-machinetoken").map(str::len), Some(43));
    // The signature still verifies with the extra headers present.
    assert_eq!(request.header("user-agent"), Some("Go-http-client/2.0"));
}

#[tokio::test]
async fn impersonation_stays_off_the_openapi_side_until_enabled() {
    let server =
        spawn_server(|_| Reply::json(200, json!({"code": "0", "data": {}}).to_string())).await;
    super::super::http::QoderApi::with_base(
        reqwest::Client::new(),
        Region::Cn,
        server.base_url.clone(),
    )
    .get_with_headers(
        "/api/v1/userinfo",
        &super::super::credential::QoderCredential::from_credential(&credential(), Region::Cn)
            .unwrap(),
        &[],
    )
    .await
    .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.header("cosy-clienttype"), None);
    assert_eq!(request.header("login-version"), None);
}

#[tokio::test]
async fn impersonation_covers_the_openapi_side_when_enabled() {
    let server =
        spawn_server(|_| Reply::json(200, json!({"code": "0", "data": {}}).to_string())).await;
    let mut api = super::super::http::QoderApi::with_base(
        reqwest::Client::new(),
        Region::Cn,
        server.base_url.clone(),
    );
    api.set_identity(crate::providers::profile::Identity::Impersonate);
    api.get_with_headers(
        "/api/v1/userinfo",
        &super::super::credential::QoderCredential::from_credential(&credential(), Region::Cn)
            .unwrap(),
        &[],
    )
    .await
    .unwrap();
    let request = &server.requests()[0];
    assert_eq!(request.header("cosy-clienttype"), Some("5"));
    assert_eq!(request.header("login-version"), Some("v2"));
    assert_eq!(request.header("cosy-clientip"), Some("169.254.198.161"));
    // Same uid ⇒ same device as the signed side.
    assert_eq!(
        request.header("cosy-machineid"),
        Some(cosy::derive_id("uid-1", "machine").as_str())
    );
}
