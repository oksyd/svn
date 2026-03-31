#[derive(Clone, Debug, Default)]
pub(super) struct FileOp {
    pub(super) action: Option<FileAction>,
    pub(super) props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
pub(super) enum FileAction {
    Put(Vec<u8>),
    Copy { from_path: String, from_rev: u64 },
}

#[derive(Clone, Debug, Default)]
pub(super) struct DirOp {
    pub(super) action: Option<DirAction>,
    pub(super) props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
pub(super) enum DirAction {
    Mkdir,
    Copy { from_path: String, from_rev: u64 },
}

#[derive(Clone, Debug)]
pub(super) struct PropChange {
    pub(super) name: String,
    pub(super) value: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(super) struct CopyOp {
    pub(super) from_path: String,
    pub(super) from_rev: Option<u64>,
    pub(super) to_path: String,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedFile {
    pub(super) path: String,
    pub(super) exists: bool,
    pub(super) action: Option<FileAction>,
    pub(super) props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
pub(super) enum Task {
    Dir(String),
    File(String),
    Delete(String),
}

impl Task {
    pub(super) fn path(&self) -> &str {
        match self {
            Task::Dir(path) | Task::File(path) | Task::Delete(path) => path.as_str(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum DirPlanKind {
    Open,
    Add { copy_from: Option<(String, u64)> },
}
