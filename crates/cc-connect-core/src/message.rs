//! Message — the chat substrate's atomic unit.
//!
//! See `PROTOCOL.md` §4 (Message schema), §10 (reserved kinds), and §11.2
//! (canonical encoding conformance vector).

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

/// Wire version. v0.1 messages MUST carry `v: 1`.
pub const PROTOCOL_VERSION: u32 = 1;

/// PROTOCOL.md §4: `body` MUST be ≤ 8 KiB after UTF-8 encoding.
pub const BODY_MAX_BYTES: usize = 8 * 1024;

/// 26-character Crockford base32 ULID.
pub const ULID_LEN: usize = 26;

/// The default (and v0.1's only) `kind`.
pub const KIND_CHAT: &str = "chat";

/// v0.2: file_drop announces an iroh-blobs hash; the bytes are fetched
/// out-of-band over the iroh-blobs ALPN against the author's NodeId.
pub const KIND_FILE_DROP: &str = "file_drop";

/// Hard ceiling on advertised file size (bytes). 1 GiB is enough for the
/// "share a screenshot / repo tarball / model weight" use-case while still
/// blocking obvious griefing payloads. Receivers MUST refuse downloads above
/// this — local disk and gossip envelope size are independent here because
/// the bytes flow over iroh-blobs, not gossip.
pub const FILE_DROP_MAX_BYTES: u64 = 1 << 30;

/// Length of a hex-encoded BLAKE3 hash (iroh-blobs Hash on the wire).
pub const BLOB_HASH_HEX_LEN: usize = 64;

/// Self-declared nickname cap. Receivers MUST drop Messages whose `nick`
/// exceeds this. Picked to keep the chatroom prefix narrow on real
/// terminals — 64 bytes leaves plenty of room for emoji + Han characters.
pub const NICK_MAX_BYTES: usize = 64;

/// A v0.1 chat Message.
///
/// Field order in the struct matches PROTOCOL.md §4's canonical JSON order
/// (`v, id, author, ts, body, kind`); serde preserves this on emit. `kind`
/// is omitted from the wire form when it equals the default `"chat"` to
/// match the §11.2 vector exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub v: u32,
    pub id: String,
    pub author: String,
    pub ts: i64,
    /// For `kind=chat`: the message body (≤ BODY_MAX_BYTES).
    /// For `kind=file_drop`: the original filename (no path components).
    pub body: String,
    #[serde(default = "default_kind", skip_serializing_if = "is_default_kind")]
    pub kind: String,
    /// v0.2 self-declared display name. Optional — receivers fall back to a
    /// short prefix of `author` when absent. Capped at `NICK_MAX_BYTES`;
    /// must contain no control characters. Forward-compatible: v0.1
    /// receivers ignore this field per §4 unknown-fields rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nick: Option<String>,
    /// v0.2 file_drop: 64-char lowercase hex iroh-blobs BLAKE3 hash. Receivers
    /// dial `author`'s NodeId over the iroh-blobs ALPN to fetch the bytes.
    /// `None` for chat messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_hash: Option<String>,
    /// v0.2 file_drop: advertised file size in bytes. Receivers MUST refuse
    /// downloads where this exceeds `FILE_DROP_MAX_BYTES`. `None` for chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_size: Option<u64>,
}

fn default_kind() -> String {
    KIND_CHAT.to_string()
}
fn is_default_kind(k: &String) -> bool {
    k == KIND_CHAT
}

impl Message {
    /// Construct a chat Message; caller supplies the ULID and timestamp.
    /// Validates body size and ULID well-formedness; normalises the ULID
    /// per Crockford rules (PROTOCOL.md §4).
    pub fn new(id: &str, author: String, ts: i64, body: String) -> Result<Self> {
        validate_body(&body)?;
        let id = normalize_ulid(id)?;
        Ok(Self {
            v: PROTOCOL_VERSION,
            id,
            author,
            ts,
            body,
            kind: KIND_CHAT.to_string(),
            nick: None,
            blob_hash: None,
            blob_size: None,
        })
    }

    /// Builder: attach a self-declared nickname (v0.2). Validates per
    /// `validate_nick`. Pass `None` to clear an existing nick.
    pub fn with_nick(mut self, nick: Option<String>) -> Result<Self> {
        if let Some(ref n) = nick {
            validate_nick(n)?;
        }
        self.nick = nick;
        Ok(self)
    }

    /// Construct a file_drop Message (v0.2).
    ///
    /// `filename` is the original basename (no path separators); `blob_hash`
    /// is a 64-char lowercase hex BLAKE3 hash returned by
    /// `iroh_blobs::store::*::add_path`; `blob_size` is the file size in
    /// bytes (must be ≤ `FILE_DROP_MAX_BYTES`). The actual bytes flow
    /// out-of-band over the iroh-blobs ALPN — this Message is only the
    /// announcement.
    pub fn new_file_drop(
        id: &str,
        author: String,
        ts: i64,
        filename: String,
        blob_hash: String,
        blob_size: u64,
    ) -> Result<Self> {
        validate_filename(&filename)?;
        validate_blob_hash(&blob_hash)?;
        validate_blob_size(blob_size)?;
        let id = normalize_ulid(id)?;
        Ok(Self {
            v: PROTOCOL_VERSION,
            id,
            author,
            ts,
            body: filename,
            kind: KIND_FILE_DROP.to_string(),
            nick: None,
            blob_hash: Some(blob_hash),
            blob_size: Some(blob_size),
        })
    }

    /// Serialise to PROTOCOL.md §4 canonical bytes.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>> {
        // serde_json's compact output (default) matches §4:
        //   - no insignificant whitespace
        //   - no `\/` escape, no HTML-escape (`<`, `>`, `&` raw)
        //   - named C0 escapes for \b \t \n \f \r, `\u00xx` for other 0x00–0x1F
        //   - DEL (0x7F) and UTF-8 multi-byte raw
        //   - integer numeric `ts` (we use i64; serde_json emits without exponent)
        Ok(serde_json::to_vec(self)?)
    }

    /// Parse a Message from wire bytes (gossip event payload or a JSONL line).
    ///
    /// Performs PROTOCOL.md §4 receiver checks in this order:
    ///   1. JSON parse → reject malformed.
    ///   2. `v` precedence (PROTOCOL §0): reject any `v != 1` *before* the
    ///      unknown-field-tolerance rule applies.
    ///   3. `kind` reservation (§10): v0.1 drops Messages with non-chat kind.
    ///   4. Body cap.
    ///   5. ULID normalise.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self> {
        let mut msg: Message = serde_json::from_slice(bytes)
            .map_err(|e| anyhow!("PARSE_ERROR: {e}"))?;

        if msg.v != PROTOCOL_VERSION {
            bail!(
                "VERSION_MISMATCH: receivers MUST drop messages with v != {PROTOCOL_VERSION} (got v={})",
                msg.v
            );
        }
        match msg.kind.as_str() {
            KIND_CHAT => validate_body(&msg.body)?,
            KIND_FILE_DROP => {
                validate_filename(&msg.body)?;
                let hash = msg
                    .blob_hash
                    .as_deref()
                    .ok_or_else(|| anyhow!("BLOB_HASH_MISSING: file_drop requires blob_hash"))?;
                validate_blob_hash(hash)?;
                let size = msg
                    .blob_size
                    .ok_or_else(|| anyhow!("BLOB_SIZE_MISSING: file_drop requires blob_size"))?;
                validate_blob_size(size)?;
            }
            other => {
                bail!(
                    "UNKNOWN_KIND: receivers MUST drop messages with unrecognised kind (got kind={other:?})"
                )
            }
        }
        if let Some(ref n) = msg.nick {
            validate_nick(n)?;
        }
        msg.id = normalize_ulid(&msg.id)?;
        Ok(msg)
    }
}

fn validate_body(body: &str) -> Result<()> {
    if body.len() > BODY_MAX_BYTES {
        bail!(
            "BODY_TOO_LARGE: {} bytes exceeds the {} byte cap (PROTOCOL.md §4)",
            body.len(),
            BODY_MAX_BYTES
        );
    }
    Ok(())
}

fn validate_filename(filename: &str) -> Result<()> {
    if filename.is_empty() {
        bail!("FILENAME_EMPTY: file_drop body must be a non-empty filename");
    }
    if filename.contains('/') || filename.contains('\\') || filename.contains('\0') {
        bail!("FILENAME_INVALID: file_drop body MUST NOT contain path separators or NUL");
    }
    Ok(())
}

fn validate_blob_hash(hash: &str) -> Result<()> {
    if hash.len() != BLOB_HASH_HEX_LEN {
        bail!(
            "BLOB_HASH_INVALID: expected {BLOB_HASH_HEX_LEN}-char hex, got {}",
            hash.len()
        );
    }
    if !hash.bytes().all(|b| b.is_ascii_hexdigit() && (!b.is_ascii_alphabetic() || b.is_ascii_lowercase())) {
        bail!("BLOB_HASH_INVALID: must be lowercase hex");
    }
    Ok(())
}

fn validate_blob_size(size: u64) -> Result<()> {
    if size > FILE_DROP_MAX_BYTES {
        bail!(
            "BLOB_TOO_LARGE: {} bytes exceeds the {} byte cap",
            size,
            FILE_DROP_MAX_BYTES
        );
    }
    Ok(())
}

fn validate_nick(nick: &str) -> Result<()> {
    if nick.is_empty() {
        bail!("NICK_EMPTY: if present, nick must be non-empty");
    }
    if nick.len() > NICK_MAX_BYTES {
        bail!(
            "NICK_TOO_LARGE: {} bytes exceeds the {} byte cap",
            nick.len(),
            NICK_MAX_BYTES
        );
    }
    if nick.chars().any(|c| c.is_control()) {
        bail!("NICK_INVALID: nick MUST NOT contain control characters");
    }
    Ok(())
}

/// Normalise a ULID per PROTOCOL.md §4's Crockford rules: case-fold to
/// uppercase; map I/i/L/l → 1, O/o → 0; reject U/u; reject everything else
/// not in the Crockford alphabet.
pub fn normalize_ulid(s: &str) -> Result<String> {
    let mut out = String::with_capacity(ULID_LEN);
    for c in s.chars() {
        let mapped = match c {
            'I' | 'i' | 'L' | 'l' => '1',
            'O' | 'o' => '0',
            'U' | 'u' => bail!("ULID_INVALID_CHAR: U/u reserved by Crockford normalisation"),
            '0'..='9' | 'A' | 'B' | 'C' | 'D' | 'E' | 'F' | 'G' | 'H' | 'J' | 'K'
            | 'M' | 'N' | 'P' | 'Q' | 'R' | 'S' | 'T' | 'V' | 'W' | 'X' | 'Y' | 'Z' => c,
            'a' | 'b' | 'c' | 'd' | 'e' | 'f' | 'g' | 'h' | 'j' | 'k' | 'm' | 'n'
            | 'p' | 'q' | 'r' | 's' | 't' | 'v' | 'w' | 'x' | 'y' | 'z' => c.to_ascii_uppercase(),
            other => bail!("ULID_INVALID_CHAR: {other:?} not in Crockford alphabet"),
        };
        out.push(mapped);
    }
    if out.len() != ULID_LEN {
        bail!(
            "ULID_LENGTH: expected {ULID_LEN} chars after normalisation, got {}",
            out.len()
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VEC_PUBKEY: &str = "hnvcppgow2sc2yvdvdicu3ynonsteflxdxrehjr2ybekdc2z3iuq";
    const VEC_ULID: &str = "01HZA8K9F0RS3JXG7QZ4N5VTBC";
    const VEC_TS: i64 = 1714323456789;
    const VEC_BODY: &str = "use postgres";

    /// PROTOCOL.md §11.2 main canonical encoding vector.
    #[test]
    fn protocol_11_2_canonical_encoding_byte_exact() {
        let msg = Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, VEC_BODY.to_string())
            .expect("valid §11.2 inputs");
        let bytes = msg.to_canonical_json().expect("canonical encode");
        let s = std::str::from_utf8(&bytes).expect("valid UTF-8");

        let expected = format!(
            r#"{{"v":1,"id":"{VEC_ULID}","author":"{VEC_PUBKEY}","ts":{VEC_TS},"body":"{VEC_BODY}"}}"#
        );
        assert_eq!(s, expected, "§11.2 canonical encoding MUST match byte-for-byte");

        // Length probe — emitted to PROTOCOL.md §11.2; spec author should
        // verify the published `Length: N bytes.` line matches this number.
        eprintln!("§11.2 canonical length = {} bytes", bytes.len());
    }

    /// PROTOCOL.md §11.2 edge-case body vector.
    /// Body raw bytes: `<` + `é` (UTF-8 0xc3 0xa9) + `>` + LF + `"` + `x`.
    /// Encoded body field including surrounding quotes is what this test
    /// pins; the spec said 12 bytes which was off by one (real value below).
    #[test]
    fn protocol_11_2_edge_body_encoding() {
        let raw = "<é>\n\"x";
        let msg = Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, raw.to_string())
            .expect("valid edge body");
        let bytes = msg.to_canonical_json().expect("encode");
        let s = std::str::from_utf8(&bytes).expect("utf-8");

        // The body field, including the surrounding quotes, MUST appear as:
        //   "<é>\n\"x"
        // where:
        //   `<`, `>`, `é` (raw UTF-8) — passed through unescaped
        //   LF (0x0a)                  — escaped as `\n` (2 ASCII bytes)
        //   `"`                        — escaped as `\"` (2 ASCII bytes)
        let expected_body_field = r#""body":"<é>\n\"x""#;
        assert!(
            s.contains(expected_body_field),
            "expected body field {expected_body_field:?} in: {s}"
        );

        // Probe — print the actual encoded length so the spec author can
        // pin the right number into PROTOCOL.md §11.2.
        eprintln!("edge body field length (incl surrounding quotes) = {}", expected_body_field.len());
    }

    #[test]
    fn roundtrip_canonical() {
        let original = Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, VEC_BODY.to_string()).unwrap();
        let bytes = original.to_canonical_json().unwrap();
        let parsed = Message::from_wire_bytes(&bytes).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn rejects_wrong_wire_version() {
        let bad = br#"{"v":2,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":""}"#;
        let err = Message::from_wire_bytes(bad).err().expect("v=2 must be rejected");
        assert!(err.to_string().contains("VERSION_MISMATCH"), "got: {err}");
    }

    #[test]
    fn rejects_unknown_kind() {
        // "system" is reserved per PROTOCOL.md §10 but not yet supported by
        // v0.2-alpha (only chat + file_drop are accepted).
        let bad = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"","kind":"system"}"#;
        let err = Message::from_wire_bytes(bad).err().expect("system kind must be rejected");
        assert!(err.to_string().contains("UNKNOWN_KIND"), "got: {err}");
    }

    /// Valid 64-char lowercase hex BLAKE3 hash (zero-vector).
    const VEC_BLOB_HASH: &str =
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

    #[test]
    fn file_drop_kind_accepted_with_filename_hash_and_size() {
        let msg = Message::new_file_drop(
            VEC_ULID,
            VEC_PUBKEY.to_string(),
            VEC_TS,
            "design.svg".to_string(),
            VEC_BLOB_HASH.to_string(),
            42,
        )
        .expect("valid file_drop");
        assert_eq!(msg.kind, KIND_FILE_DROP);
        assert_eq!(msg.body, "design.svg");
        assert_eq!(msg.blob_hash.as_deref(), Some(VEC_BLOB_HASH));
        assert_eq!(msg.blob_size, Some(42));
        let wire = msg.to_canonical_json().unwrap();
        let parsed = Message::from_wire_bytes(&wire).expect("file_drop wire roundtrip");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn file_drop_with_path_separator_in_filename_rejected() {
        let err = Message::new_file_drop(
            VEC_ULID,
            VEC_PUBKEY.to_string(),
            VEC_TS,
            "../etc/passwd".to_string(),
            VEC_BLOB_HASH.to_string(),
            1,
        )
        .err()
        .expect("path separator MUST be rejected");
        assert!(err.to_string().contains("FILENAME_INVALID"), "got: {err}");
    }

    #[test]
    fn file_drop_with_oversized_blob_rejected_at_constructor() {
        let err = Message::new_file_drop(
            VEC_ULID,
            VEC_PUBKEY.to_string(),
            VEC_TS,
            "huge.bin".to_string(),
            VEC_BLOB_HASH.to_string(),
            FILE_DROP_MAX_BYTES + 1,
        )
        .err()
        .expect("oversize MUST be rejected");
        assert!(err.to_string().contains("BLOB_TOO_LARGE"), "got: {err}");
    }

    #[test]
    fn file_drop_with_invalid_hash_rejected() {
        let err = Message::new_file_drop(
            VEC_ULID,
            VEC_PUBKEY.to_string(),
            VEC_TS,
            "x".to_string(),
            "deadbeef".to_string(),
            1,
        )
        .err()
        .expect("short hash rejected");
        assert!(err.to_string().contains("BLOB_HASH_INVALID"), "got: {err}");

        // uppercase hex not allowed (canonical lowercase)
        let upper = VEC_BLOB_HASH.to_uppercase();
        let err = Message::new_file_drop(
            VEC_ULID,
            VEC_PUBKEY.to_string(),
            VEC_TS,
            "x".to_string(),
            upper,
            1,
        )
        .err()
        .expect("uppercase hex rejected");
        assert!(err.to_string().contains("BLOB_HASH_INVALID"), "got: {err}");
    }

    #[test]
    fn file_drop_missing_blob_hash_rejected_on_wire() {
        let bad = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"f.bin","kind":"file_drop","blob_size":1}"#;
        let err = Message::from_wire_bytes(bad).err().expect("missing hash rejected");
        assert!(err.to_string().contains("BLOB_HASH_MISSING"), "got: {err}");
    }

    #[test]
    fn file_drop_missing_blob_size_rejected_on_wire() {
        let bad = format!(
            r#"{{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"f.bin","kind":"file_drop","blob_hash":"{VEC_BLOB_HASH}"}}"#,
        );
        let err = Message::from_wire_bytes(bad.as_bytes())
            .err()
            .expect("missing size rejected");
        assert!(err.to_string().contains("BLOB_SIZE_MISSING"), "got: {err}");
    }

    #[test]
    fn file_drop_with_empty_body_rejected_on_wire() {
        let bad = format!(
            r#"{{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"","kind":"file_drop","blob_hash":"{VEC_BLOB_HASH}","blob_size":1}}"#,
        );
        let err = Message::from_wire_bytes(bad.as_bytes())
            .err()
            .expect("empty filename rejected");
        assert!(err.to_string().contains("FILENAME_EMPTY"), "got: {err}");
    }

    #[test]
    fn chat_message_has_no_blob_fields_in_canonical_form() {
        let msg = Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, VEC_BODY.to_string()).unwrap();
        let s = String::from_utf8(msg.to_canonical_json().unwrap()).unwrap();
        assert!(
            !s.contains("blob_hash") && !s.contains("blob_size"),
            "chat Messages must omit blob fields (skip_serializing_if): {s}"
        );
    }

    #[test]
    fn absent_kind_treated_as_chat() {
        let m = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":""}"#;
        let parsed = Message::from_wire_bytes(m).unwrap();
        assert_eq!(parsed.kind, KIND_CHAT);
    }

    #[test]
    fn explicit_chat_kind_parses() {
        let m = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"","kind":"chat"}"#;
        let parsed = Message::from_wire_bytes(m).unwrap();
        assert_eq!(parsed.kind, KIND_CHAT);
    }

    #[test]
    fn rejects_oversized_body() {
        let body = "x".repeat(BODY_MAX_BYTES + 1);
        let r = Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, body);
        let err = r.err().expect("oversized body must be rejected");
        assert!(err.to_string().contains("BODY_TOO_LARGE"), "got: {err}");
    }

    #[test]
    fn accepts_body_at_exact_cap() {
        let body = "x".repeat(BODY_MAX_BYTES);
        Message::new(VEC_ULID, VEC_PUBKEY.to_string(), VEC_TS, body).expect("8 KiB body OK");
    }

    #[test]
    fn ignores_unknown_top_level_fields() {
        // PROTOCOL §4: receivers MUST ignore unknown top-level fields after
        // the `v` and `kind` checks pass.
        let m = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1,"body":"","future_field":42}"#;
        Message::from_wire_bytes(m).expect("unknown top-level fields tolerated");
    }

    #[test]
    fn ulid_normalizes_crockford_aliases() {
        // I, L, O are aliases of 1, 1, 0.
        let raw = "01HZA8K9F0RS3JXG7QZ4N5VTBI"; // 26 chars; trailing I → 1
        let n = normalize_ulid(raw).unwrap();
        assert_eq!(n, "01HZA8K9F0RS3JXG7QZ4N5VTB1");

        let raw = "01HZA8K9F0RS3JXG7QZ4N5VTBO"; // trailing O → 0
        let n = normalize_ulid(raw).unwrap();
        assert_eq!(n, "01HZA8K9F0RS3JXG7QZ4N5VTB0");

        let raw = "01HZA8K9F0RS3JXG7QZ4N5VTBl"; // lowercase l → 1
        let n = normalize_ulid(raw).unwrap();
        assert_eq!(n, "01HZA8K9F0RS3JXG7QZ4N5VTB1");
    }

    #[test]
    fn ulid_rejects_u_and_u_lower() {
        // Crockford reserves U/u; PROTOCOL.md §4 says reject.
        let bad = "01HZA8K9F0RS3JXG7QZ4N5VTBU";
        let err = normalize_ulid(bad).err().expect("U must be rejected");
        assert!(err.to_string().contains("U"), "got: {err}");
    }

    #[test]
    fn ulid_rejects_wrong_length() {
        let too_short = "01HZA8K9F";
        let err = normalize_ulid(too_short).err().expect("short ULID rejected");
        assert!(err.to_string().contains("ULID_LENGTH"), "got: {err}");
    }

    #[test]
    fn ulid_normalizes_lowercase_to_uppercase() {
        // Per §4 canonical output MUST be uppercase.
        let lower = "01hza8k9f0rs3jxg7qz4n5vtbc";
        let n = normalize_ulid(lower).unwrap();
        assert_eq!(n, "01HZA8K9F0RS3JXG7QZ4N5VTBC");
    }

    /// Parse-fails reject malformed JSON gracefully (no panic, error mentions PARSE).
    #[test]
    fn rejects_malformed_json() {
        let err = Message::from_wire_bytes(b"not json").err().unwrap();
        assert!(err.to_string().contains("PARSE_ERROR"), "got: {err}");
    }

    /// `ts` MUST be an integer literal; reject scientific-notation forms.
    #[test]
    fn rejects_exponent_ts() {
        let bad = br#"{"v":1,"id":"01HZA8K9F0RS3JXG7QZ4N5VTBC","author":"x","ts":1.7e12,"body":""}"#;
        // serde_json will fail to deserialise i64 from a float — that's the
        // canonical-encoding-only stance in §4. We accept the parse error.
        let err = Message::from_wire_bytes(bad).err().expect("exponent ts rejected");
        assert!(err.to_string().contains("PARSE_ERROR"), "got: {err}");
    }
}
