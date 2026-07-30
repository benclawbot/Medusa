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
