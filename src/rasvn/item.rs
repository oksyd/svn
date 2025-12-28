use std::fmt::{Display, Formatter};

use super::wire::WireEncoder;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq)]
/// A raw `ra_svn` wire protocol item.
///
/// This is intentionally low-level. Most users should prefer higher-level APIs
/// in [`crate::RaSvnClient`] / [`crate::RaSvnSession`].
pub enum SvnItem {
    /// A protocol word token.
    Word(String),
    /// A protocol number token.
    Number(u64),
    /// A protocol string token (raw bytes; may not be valid UTF-8).
    String(Vec<u8>),
    /// A protocol list token.
    List(Vec<SvnItem>),
    /// A protocol boolean token.
    Bool(bool),
}

impl SvnItem {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            SvnItem::Word(_) => "word",
            SvnItem::Number(_) => "number",
            SvnItem::String(_) => "string",
            SvnItem::List(_) => "list",
            SvnItem::Bool(_) => "bool",
        }
    }

    /// Returns this item as a `word`, if it is a word.
    ///
    /// This clones the underlying string.
    pub fn as_word(&self) -> Option<String> {
        match self {
            SvnItem::Word(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Returns this item as a `u64`, if it is a number.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            SvnItem::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Returns this item as a `bool`, if it is a boolean (or a boolean word).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            SvnItem::Bool(b) => Some(*b),
            SvnItem::Word(w) if w == "true" => Some(true),
            SvnItem::Word(w) if w == "false" => Some(false),
            _ => None,
        }
    }

    /// Returns this item as a UTF-8 string, if it is a `string` and is valid UTF-8.
    ///
    /// For binary strings, use [`SvnItem::as_bytes_string`].
    pub fn as_string(&self) -> Option<String> {
        match self {
            SvnItem::String(bytes) => String::from_utf8(bytes.clone()).ok(),
            _ => None,
        }
    }

    /// Returns this item as raw bytes, if it is a `string`.
    ///
    /// This clones the underlying byte buffer.
    pub fn as_bytes_string(&self) -> Option<Vec<u8>> {
        match self {
            SvnItem::String(bytes) => Some(bytes.clone()),
            _ => None,
        }
    }

    /// Returns this item as a list, if it is a `list`.
    ///
    /// This clones the underlying vector.
    pub fn as_list(&self) -> Option<Vec<SvnItem>> {
        match self {
            SvnItem::List(items) => Some(items.clone()),
            _ => None,
        }
    }
}

impl Display for SvnItem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SvnItem::Word(w) => write!(f, "{w}"),
            SvnItem::Number(n) => write!(f, "{n}"),
            SvnItem::String(s) => write!(f, "<{} bytes>", s.len()),
            SvnItem::List(items) => write!(f, "({} items)", items.len()),
            SvnItem::Bool(b) => write!(f, "{b}"),
        }
    }
}

pub(crate) fn encode_item(item: &SvnItem, out: &mut Vec<u8>) {
    let mut enc = WireEncoder::new(out);
    encode_item_with(&mut enc, item);
}

fn encode_item_with(enc: &mut WireEncoder<'_>, item: &SvnItem) {
    match item {
        SvnItem::Word(w) => enc.word(w),
        SvnItem::Number(n) => enc.number(*n),
        SvnItem::Bool(b) => enc.bool(*b),
        SvnItem::String(s) => enc.string_bytes(s),
        SvnItem::List(items) => {
            enc.list_start();
            for item in items {
                encode_item_with(enc, item);
            }
            enc.list_end();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn codec_encodes_expected_bytes() {
        let item = SvnItem::List(vec![
            SvnItem::Word("word".to_string()),
            SvnItem::Number(22),
            SvnItem::String(b"string".to_vec()),
            SvnItem::List(vec![SvnItem::Word("sublist".to_string())]),
        ]);

        let mut bytes = Vec::new();
        encode_item(&item, &mut bytes);
        assert_eq!(bytes, b"( word 22 6:string ( sublist ) ) ");
    }

    #[test]
    fn handshake_response_encodes_trailing_empty_client_list() {
        let item = SvnItem::List(vec![
            SvnItem::Number(2),
            SvnItem::List(vec![
                SvnItem::Word("edit-pipeline".to_string()),
                SvnItem::Word("svndiff1".to_string()),
                SvnItem::Word("accepts-svndiff2".to_string()),
                SvnItem::Word("absent-entries".to_string()),
                SvnItem::Word("depth".to_string()),
                SvnItem::Word("mergeinfo".to_string()),
                SvnItem::Word("log-revprops".to_string()),
            ]),
            SvnItem::String(b"svn://example.com:3690/repo".to_vec()),
            SvnItem::String(b"prototype-ra_svn".to_vec()),
            SvnItem::List(Vec::new()),
        ]);

        let mut bytes = Vec::new();
        encode_item(&item, &mut bytes);
        assert_eq!(
            bytes,
            b"( 2 ( edit-pipeline svndiff1 accepts-svndiff2 absent-entries depth mergeinfo log-revprops ) 27:svn://example.com:3690/repo 16:prototype-ra_svn ( ) ) "
        );
    }

    #[test]
    fn auth_response_cram_md5_encodes_empty_token_tuple() {
        let item = SvnItem::List(vec![
            SvnItem::Word("CRAM-MD5".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let mut bytes = Vec::new();
        encode_item(&item, &mut bytes);
        assert_eq!(bytes, b"( CRAM-MD5 ( ) ) ");
    }

    #[test]
    fn auth_response_anonymous_encodes_empty_string_token() {
        let item = SvnItem::List(vec![
            SvnItem::Word("ANONYMOUS".to_string()),
            SvnItem::List(vec![SvnItem::String(Vec::new())]),
        ]);

        let mut bytes = Vec::new();
        encode_item(&item, &mut bytes);
        assert_eq!(bytes, b"( ANONYMOUS ( 0: ) ) ");
    }

    #[test]
    fn auth_response_plain_encodes_binary_token_inside_tuple() {
        let item = SvnItem::List(vec![
            SvnItem::Word("PLAIN".to_string()),
            SvnItem::List(vec![SvnItem::String(vec![0, b'u', 0, b'p'])]),
        ]);

        let mut bytes = Vec::new();
        encode_item(&item, &mut bytes);

        let mut expected = b"( PLAIN ( 4:".to_vec();
        expected.extend_from_slice(&[0, b'u', 0, b'p']);
        expected.extend_from_slice(b" ) ) ");
        assert_eq!(bytes, expected);
    }
}
