use super::*;

/// A fully recorded textdelta stream for one file token.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedTextDelta {
    /// Repository-relative path, if known (from `open-file` / `add-file`).
    pub path: Option<String>,
    /// File token associated with this delta stream.
    pub file_token: String,
    /// Base checksum announced by the server (if any).
    pub base_checksum: Option<String>,
    /// Raw svndiff chunks as received from the server.
    pub chunks: Vec<Vec<u8>>,
    /// Optional text checksum announced on `close-file`.
    pub text_checksum: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct PendingTextDelta {
    path: Option<String>,
    base_checksum: Option<String>,
    chunks: Vec<Vec<u8>>,
}

/// Records `apply-textdelta` streams from an editor drive.
///
/// This is a helper for `update`/`diff`/`replay`-style operations where the
/// server emits `apply-textdelta` and `textdelta-chunk` events. The recorder
/// stores raw svndiff chunks, which can later be applied with
/// [`apply_textdelta`].
///
/// This collector is in-memory and may use significant RAM for large edits.
#[derive(Debug, Default)]
pub struct TextDeltaRecorder {
    file_paths: HashMap<String, String>,
    pending: HashMap<String, PendingTextDelta>,
    last_completed: HashMap<String, usize>,
    completed: Vec<RecordedTextDelta>,
}

impl TextDeltaRecorder {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns all completed textdeltas recorded so far.
    pub fn completed(&self) -> &[RecordedTextDelta] {
        &self.completed
    }

    /// Takes all completed textdeltas, leaving the recorder empty.
    pub fn take_completed(&mut self) -> Vec<RecordedTextDelta> {
        self.last_completed.clear();
        std::mem::take(&mut self.completed)
    }
}

impl EditorEventHandler for TextDeltaRecorder {
    fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
        match event {
            EditorEvent::AddFile {
                path, file_token, ..
            }
            | EditorEvent::OpenFile {
                path, file_token, ..
            } => {
                self.file_paths.insert(file_token, path);
            }
            EditorEvent::ApplyTextDelta {
                file_token,
                base_checksum,
            } => {
                if self.pending.contains_key(&file_token) {
                    return Err(SvnError::Protocol(format!(
                        "duplicate apply-textdelta for file token '{file_token}'"
                    )));
                }

                let path = self.file_paths.get(&file_token).cloned();
                self.pending.insert(
                    file_token,
                    PendingTextDelta {
                        path,
                        base_checksum,
                        chunks: Vec::new(),
                    },
                );
            }
            EditorEvent::TextDeltaChunk { file_token, chunk } => {
                let pending = self.pending.get_mut(&file_token).ok_or_else(|| {
                    SvnError::Protocol(format!(
                        "textdelta-chunk for unknown file token '{file_token}'"
                    ))
                })?;
                pending.chunks.push(chunk);
            }
            EditorEvent::TextDeltaEnd { file_token } => {
                let pending = self.pending.remove(&file_token).ok_or_else(|| {
                    SvnError::Protocol(format!(
                        "textdelta-end for unknown file token '{file_token}'"
                    ))
                })?;

                let record = RecordedTextDelta {
                    path: pending.path,
                    file_token: file_token.clone(),
                    base_checksum: pending.base_checksum,
                    chunks: pending.chunks,
                    text_checksum: None,
                };
                self.completed.push(record);
                self.last_completed
                    .insert(file_token, self.completed.len() - 1);
            }
            EditorEvent::CloseFile {
                file_token,
                text_checksum,
            } => {
                if let Some(text_checksum) = text_checksum
                    && let Some(&idx) = self.last_completed.get(&file_token)
                    && let Some(record) = self.completed.get_mut(idx)
                {
                    record.text_checksum = Some(text_checksum);
                }
                self.file_paths.remove(&file_token);
            }
            EditorEvent::CloseEdit | EditorEvent::AbortEdit | EditorEvent::FinishReplay
                if !self.pending.is_empty() =>
            {
                return Err(SvnError::Protocol(
                    "editor drive ended with an unfinished textdelta".into(),
                ));
            }
            EditorEvent::CloseEdit | EditorEvent::AbortEdit | EditorEvent::FinishReplay => {}
            _ => {}
        }

        Ok(())
    }
}
