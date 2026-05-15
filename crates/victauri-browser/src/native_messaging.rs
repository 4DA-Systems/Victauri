use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_INPUT_SIZE: usize = 1_048_576;
const MAX_OUTPUT_SIZE: usize = 67_108_864;

/// Read one native messaging frame from stdin.
///
/// Chrome sends: 4-byte little-endian length prefix, then UTF-8 JSON.
/// Maximum input size is 1 MB per Chrome's spec.
///
/// # Errors
///
/// Returns an error if the message exceeds the 1 MB limit, if the read
/// fails, or if the bytes are not valid JSON.
pub async fn read_message(
    reader: &mut (impl AsyncReadExt + Unpin),
) -> Result<serde_json::Value, NativeMessageError> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            NativeMessageError::Disconnected
        } else {
            NativeMessageError::Io(e)
        }
    })?;

    let len = u32::from_le_bytes(len_bytes) as usize;
    if len > MAX_INPUT_SIZE {
        return Err(NativeMessageError::TooLarge {
            size: len,
            max: MAX_INPUT_SIZE,
        });
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await.map_err(NativeMessageError::Io)?;

    serde_json::from_slice(&buf).map_err(NativeMessageError::Json)
}

/// Write one native messaging frame to stdout.
///
/// Format: 4-byte little-endian length prefix, then UTF-8 JSON.
/// Maximum output size is 64 MB per Chrome's spec (host → extension direction).
///
/// # Errors
///
/// Returns an error if the serialized message exceeds 64 MB or if the write fails.
pub async fn write_message(
    writer: &mut (impl AsyncWriteExt + Unpin),
    msg: &serde_json::Value,
) -> Result<(), NativeMessageError> {
    let bytes = serde_json::to_vec(msg).map_err(NativeMessageError::Json)?;
    if bytes.len() > MAX_OUTPUT_SIZE {
        return Err(NativeMessageError::TooLarge {
            size: bytes.len(),
            max: MAX_OUTPUT_SIZE,
        });
    }

    let len_bytes = (bytes.len() as u32).to_le_bytes();
    writer.write_all(&len_bytes).await.map_err(NativeMessageError::Io)?;
    writer.write_all(&bytes).await.map_err(NativeMessageError::Io)?;
    writer.flush().await.map_err(NativeMessageError::Io)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum NativeMessageError {
    #[error("native messaging peer disconnected")]
    Disconnected,

    #[error("message size {size} exceeds limit {max}")]
    TooLarge { size: usize, max: usize },

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn roundtrip_message() {
        let msg = serde_json::json!({"type": "execute", "id": "abc123", "method": "snapshot"});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();

        assert_eq!(msg, decoded);
    }

    #[tokio::test]
    async fn rejects_oversized_input() {
        let len = (MAX_INPUT_SIZE + 1) as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend(vec![0u8; MAX_INPUT_SIZE + 1]);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn detects_disconnect() {
        let buf: &[u8] = &[];
        let mut reader = BufReader::new(buf);
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Disconnected)));
    }

    #[tokio::test]
    async fn handles_empty_json() {
        let msg = serde_json::json!({});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded, serde_json::json!({}));
    }

    #[tokio::test]
    async fn handles_multiple_messages() {
        let msgs = vec![
            serde_json::json!({"id": "1"}),
            serde_json::json!({"id": "2", "data": "hello"}),
            serde_json::json!({"id": "3", "nested": {"key": "value"}}),
        ];

        let mut buf = Vec::new();
        for msg in &msgs {
            write_message(&mut buf, msg).await.unwrap();
        }

        let mut reader = BufReader::new(buf.as_slice());
        for expected in &msgs {
            let decoded = read_message(&mut reader).await.unwrap();
            assert_eq!(&decoded, expected);
        }
    }

    #[tokio::test]
    async fn partial_length_prefix_is_disconnect() {
        let buf: &[u8] = &[0x02, 0x00];
        let mut reader = BufReader::new(buf);
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Disconnected)));
    }

    #[tokio::test]
    async fn invalid_json_returns_error() {
        let invalid = b"not json at all";
        let len = invalid.len() as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(invalid);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Json(_))));
    }

    #[tokio::test]
    async fn zero_length_message() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Json(_))));
    }

    #[tokio::test]
    async fn unicode_message_roundtrip() {
        let msg = serde_json::json!({"emoji": "🔥🚀", "cjk": "日本語テスト", "mixed": "hello 世界"});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded["emoji"], "🔥🚀");
        assert_eq!(decoded["cjk"], "日本語テスト");
    }

    #[tokio::test]
    async fn large_message_near_limit() {
        let big_string = "x".repeat(500_000);
        let msg = serde_json::json!({"data": big_string});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded["data"].as_str().unwrap().len(), 500_000);
    }

    #[tokio::test]
    async fn write_message_length_prefix_correct() {
        let msg = serde_json::json!({"a": 1});
        let expected_json = serde_json::to_vec(&msg).unwrap();

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(len, expected_json.len());
        assert_eq!(&buf[4..], &expected_json);
    }

    #[tokio::test]
    async fn truncated_body_is_io_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u32.to_le_bytes());
        buf.extend_from_slice(b"short");

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Io(_))));
    }

    // --- Adversarial stress tests ---

    #[tokio::test]
    async fn message_at_exact_1mb_boundary_rejected() {
        let len = MAX_INPUT_SIZE as u32 + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend(vec![b'x'; MAX_INPUT_SIZE + 1]);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn message_at_exact_1mb_minus_one_accepted_if_valid_json() {
        let padding = "x".repeat(MAX_INPUT_SIZE - 20);
        let json_str = format!(r#"{{"d":"{padding}"}}"#);
        let json_bytes = json_str.as_bytes();
        assert!(json_bytes.len() <= MAX_INPUT_SIZE);

        let mut buf = Vec::new();
        buf.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(json_bytes);

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded["d"].as_str().unwrap().len(), padding.len());
    }

    #[tokio::test]
    async fn deeply_nested_json_1000_levels() {
        let mut json = String::from("null");
        for _ in 0..1000 {
            json = format!(r#"{{"n":{json}}}"#);
        }
        let json_bytes = json.as_bytes();
        assert!(json_bytes.len() <= MAX_INPUT_SIZE);

        let mut buf = Vec::new();
        buf.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(json_bytes);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        // serde_json has a recursion limit of 128 by default
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn json_with_null_bytes_in_strings() {
        let msg = serde_json::json!({"data": "hello\u{0000}world"});
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert!(decoded["data"].as_str().unwrap().contains('\u{0000}'));
    }

    #[tokio::test]
    async fn rapid_sequential_50_messages() {
        let mut buf = Vec::new();
        for i in 0..50 {
            let msg = serde_json::json!({"seq": i, "ts": "2024-01-01T00:00:00Z"});
            write_message(&mut buf, &msg).await.unwrap();
        }

        let mut reader = BufReader::new(buf.as_slice());
        for i in 0..50 {
            let decoded = read_message(&mut reader).await.unwrap();
            assert_eq!(decoded["seq"], i);
        }
        let eof = read_message(&mut reader).await;
        assert!(matches!(eof, Err(NativeMessageError::Disconnected)));
    }

    #[tokio::test]
    async fn json_with_10000_keys() {
        let mut map = serde_json::Map::new();
        for i in 0..10_000 {
            map.insert(format!("key_{i}"), serde_json::Value::Number(i.into()));
        }
        let msg = serde_json::Value::Object(map);

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded.as_object().unwrap().len(), 10_000);
    }

    #[tokio::test]
    async fn output_at_64mb_limit_rejected() {
        let big = "x".repeat(MAX_OUTPUT_SIZE + 1);
        let msg = serde_json::json!({"huge": big});

        let mut buf = Vec::new();
        let result = write_message(&mut buf, &msg).await;
        assert!(matches!(result, Err(NativeMessageError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn message_with_all_json_value_types() {
        let msg = serde_json::json!({
            "null": null,
            "bool_true": true,
            "bool_false": false,
            "int": 42,
            "float": 1.23456,
            "negative": -999,
            "string": "hello",
            "array": [1, "two", null, [3]],
            "object": {"nested": {"deep": true}},
            "empty_array": [],
            "empty_object": {},
            "big_int": 9007199254740992_i64,
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn length_prefix_u32_max_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        buf.extend(vec![0u8; 100]);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn interleaved_write_read_sequence() {
        let mut buf = Vec::new();

        let msg1 = serde_json::json!({"phase": "init"});
        write_message(&mut buf, &msg1).await.unwrap();
        let msg2 = serde_json::json!({"phase": "execute", "data": [1,2,3]});
        write_message(&mut buf, &msg2).await.unwrap();
        let msg3 = serde_json::json!({"phase": "complete", "ok": true});
        write_message(&mut buf, &msg3).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).await.unwrap()["phase"], "init");
        assert_eq!(read_message(&mut reader).await.unwrap()["phase"], "execute");
        assert_eq!(read_message(&mut reader).await.unwrap()["phase"], "complete");
    }

    #[tokio::test]
    async fn unicode_surrogate_edge_cases() {
        let msg = serde_json::json!({
            "emoji_sequence": "👨‍👩‍👧‍👦",
            "zalgo": "h̷̡̢̧e̵̢̧̛l̸̨̧̛l̵̡̢̧ơ̷̢̧",
            "rtl": "مرحبا",
            "combining": "a\u{0300}\u{0301}\u{0302}",
        });

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded["emoji_sequence"], "👨‍👩‍👧‍👦");
    }

    // --- Deep challenger: protocol edge cases ---

    #[tokio::test]
    async fn truncated_body_causes_io_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u32.to_le_bytes());
        buf.extend(vec![b'x'; 50]);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Io(_))));
    }

    #[tokio::test]
    async fn zero_length_message_is_invalid_json() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Json(_))));
    }

    #[tokio::test]
    async fn partial_length_prefix_causes_disconnect() {
        let buf = [0x10, 0x00];
        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Disconnected)));
    }

    #[tokio::test]
    async fn write_then_read_back_large_near_limit() {
        let big_string = "a".repeat(900_000);
        let msg = serde_json::json!({"data": big_string});

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        let decoded = read_message(&mut reader).await.unwrap();
        assert_eq!(decoded["data"].as_str().unwrap().len(), 900_000);
    }

    #[tokio::test]
    async fn multiple_messages_then_disconnect() {
        let mut buf = Vec::new();
        write_message(&mut buf, &serde_json::json!({"n": 1})).await.unwrap();
        write_message(&mut buf, &serde_json::json!({"n": 2})).await.unwrap();

        let mut reader = BufReader::new(buf.as_slice());
        assert_eq!(read_message(&mut reader).await.unwrap()["n"], 1);
        assert_eq!(read_message(&mut reader).await.unwrap()["n"], 2);
        assert!(matches!(read_message(&mut reader).await, Err(NativeMessageError::Disconnected)));
    }

    #[tokio::test]
    async fn invalid_utf8_in_body_is_json_error() {
        let invalid = [0xFF, 0xFE, 0xFD, 0xFC, 0xFB];
        let mut buf = Vec::new();
        buf.extend_from_slice(&(invalid.len() as u32).to_le_bytes());
        buf.extend_from_slice(&invalid);

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(matches!(result, Err(NativeMessageError::Json(_))));
    }

    #[tokio::test]
    async fn valid_json_non_object_types() {
        let values: Vec<serde_json::Value> = vec![
            serde_json::json!([1, 2, 3]),
            serde_json::json!("just a string"),
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::json!(null),
        ];

        for val in values {
            let mut buf = Vec::new();
            write_message(&mut buf, &val).await.unwrap();

            let mut reader = BufReader::new(buf.as_slice());
            let decoded = read_message(&mut reader).await.unwrap();
            assert_eq!(decoded, val);
        }
    }

    #[tokio::test]
    async fn length_prefix_exactly_at_boundary() {
        let json_body = format!("{{\"x\":\"{}\"}}", "a".repeat(MAX_INPUT_SIZE - 8));
        assert!(json_body.len() <= MAX_INPUT_SIZE);

        let mut buf = Vec::new();
        buf.extend_from_slice(&(json_body.len() as u32).to_le_bytes());
        buf.extend_from_slice(json_body.as_bytes());

        let mut reader = BufReader::new(buf.as_slice());
        let result = read_message(&mut reader).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn write_verifies_length_prefix_correctness() {
        let big_val = "x".repeat(10_000_000);
        let msg = serde_json::json!({"data": big_val});

        let mut buf = Vec::new();
        let result = write_message(&mut buf, &msg).await;
        assert!(result.is_ok());
        let written_len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        assert_eq!(written_len + 4, buf.len());
    }
}
