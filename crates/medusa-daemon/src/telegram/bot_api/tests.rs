use super::types::{TelegramTransportUpdate, TelegramUpdate, TelegramUpdateCursor};
use super::{TelegramBotApiError, TelegramBotToken, TelegramTransportFailure};

#[test]
fn token_debug_never_exposes_secret() {
    let token = TelegramBotToken::new("123456:abc_DEF-789").expect("valid token");
    let debug = format!("{token:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("abc_DEF-789"));
}

#[test]
fn token_validation_rejects_whitespace_and_malformed_values() {
    for token in [" 123:abc", "123:abc ", "abc:def", "123:", "123:a/b"] {
        assert_eq!(
            TelegramBotToken::new(token),
            Err(TelegramBotApiError::InvalidToken)
        );
    }
}

#[test]
fn cursor_advances_monotonically_and_rejects_regression() {
    let mut cursor = TelegramUpdateCursor::default();
    cursor.acknowledge(7).expect("acknowledge");
    assert_eq!(cursor.next_offset(), Some(8));
    cursor.acknowledge(8).expect("acknowledge");
    assert_eq!(cursor.next_offset(), Some(9));
    assert_eq!(
        cursor.acknowledge(6),
        Err(TelegramBotApiError::InvalidUpdate)
    );
}

#[test]
fn unsupported_updates_remain_acknowledgeable() {
    let update = TelegramUpdate {
        update_id: 11,
        message: None,
        callback_query: None,
    };
    assert_eq!(
        TelegramTransportUpdate::try_from(update).expect("typed update"),
        TelegramTransportUpdate::Unsupported { update_id: 11 }
    );
}

#[test]
fn transient_classification_is_explicit() {
    assert!(TelegramBotApiError::RetryAfter { seconds: 3 }.is_transient());
    assert!(
        TelegramBotApiError::Transport {
            kind: TelegramTransportFailure::Timeout,
            status: None,
        }
        .is_transient()
    );
    assert!(!TelegramBotApiError::InvalidToken.is_transient());
}

#[test]
fn media_updates_deserialize_with_album_and_document_metadata() {
    let update: TelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 12,
        "message": {
            "message_id": 7,
            "date": 1700000000,
            "media_group_id": "album-1",
            "chat": {"id": 42, "type": "private"},
            "from": {"id": 42, "is_bot": false, "first_name": "Ada"},
            "photo": [{
                "file_id": "photo-file",
                "file_unique_id": "photo-unique",
                "width": 320,
                "height": 200,
                "file_size": 1000
            }],
            "document": {
                "file_id": "document-file",
                "file_unique_id": "document-unique",
                "file_name": "notes.txt",
                "mime_type": "text/plain",
                "file_size": 20
            },
            "caption": "inspect"
        }
    }))
    .expect("deserialize media update");
    let message = update.message.expect("message");
    assert_eq!(message.media_group_id.as_deref(), Some("album-1"));
    assert_eq!(message.photo.len(), 1);
    assert_eq!(
        message.document.and_then(|document| document.file_name),
        Some("notes.txt".to_owned())
    );
}

#[test]
fn file_paths_reject_traversal_and_accept_telegram_paths() {
    assert!(super::validate_file_path("photos/file_0.jpg").is_ok());
    assert!(super::validate_file_path("../secret").is_err());
    assert!(super::validate_file_path("photos//file.jpg").is_err());
}
