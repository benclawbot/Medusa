use medusa_browser_client::{BrowserRequest, BrowserResponse, ElementRef, TabInfo};
use serde_json::json;

#[test]
fn every_browser_request_round_trips_through_json() {
    let requests = vec![
        BrowserRequest::Ping,
        BrowserRequest::Navigate {
            url: "https://example.com".into(),
        },
        BrowserRequest::Snapshot,
        BrowserRequest::Click {
            ref_id: Some(7),
            selector: None,
        },
        BrowserRequest::Click {
            ref_id: None,
            selector: Some("#submit".into()),
        },
        BrowserRequest::Fill {
            ref_id: Some(9),
            selector: Some("input[name=q]".into()),
            value: "medusa".into(),
        },
        BrowserRequest::Press {
            key: "Enter".into(),
        },
        BrowserRequest::Screenshot { full_page: true },
        BrowserRequest::Evaluate {
            expression: "document.title".into(),
        },
        BrowserRequest::Tabs,
        BrowserRequest::Close,
    ];

    for request in requests {
        let encoded = serde_json::to_string(&request).expect("serialize request");
        let decoded: BrowserRequest = serde_json::from_str(&encoded).expect("deserialize request");
        assert_eq!(decoded, request);
    }
}

#[test]
fn every_browser_response_round_trips_and_reports_error_state() {
    let element = ElementRef {
        id: 1,
        role: "button".into(),
        name: "Submit".into(),
        selector: "#submit".into(),
    };
    let tab = TabInfo {
        id: 3,
        url: "https://example.com".into(),
        title: "Example".into(),
    };
    let responses = vec![
        BrowserResponse::Ok,
        BrowserResponse::Navigate {
            final_url: "https://example.com/home".into(),
            status: 200,
        },
        BrowserResponse::Snapshot {
            text: "Submit".into(),
            refs: vec![element],
        },
        BrowserResponse::Screenshot {
            format: "png".into(),
            bytes_base64: "AAEC".into(),
        },
        BrowserResponse::Evaluate {
            value: json!({"ready": true}),
        },
        BrowserResponse::Tabs { tabs: vec![tab] },
        BrowserResponse::Error {
            code: "not_found".into(),
            message: "missing element".into(),
        },
    ];

    for response in responses {
        let expected_ok = !matches!(response, BrowserResponse::Error { .. });
        assert_eq!(response.is_ok(), expected_ok);
        let encoded = serde_json::to_string(&response).expect("serialize response");
        let decoded: BrowserResponse = serde_json::from_str(&encoded).expect("deserialize response");
        assert_eq!(decoded, response);
    }
}
